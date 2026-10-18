# 日志与留存契约

日期：2026-10-16。状态：已实现并配备运行测试（[tests/logging.rs](../../tests/logging.rs)、[tests/logging_process.rs](../../tests/logging_process.rs)、[tests/configuration.rs](../../tests/configuration.rs)）。日志是运维扩展，不属于 CloudKit 同步协议，也不参与同步 token、快照、租约或资源语义。

## 管线与等级

一个 `tracing` subscriber（[src/logging](../../src/logging/mod.rs)）同时负责渲染与落库。同一条记录只经过一次脱敏，因此控制台与数据库不可能出现不同的字段可见性。

| 配置 | 默认 | 语义 |
|---|---|---|
| SYNC_LOG_LEVEL | `info` | 控制台最低等级；`off` 不打印 |
| SYNC_LOG_PERSIST_LEVEL | 继承 SYNC_LOG_LEVEL | 落库最低等级；`off` 时不创建也不打开 `logs.sqlite` |
| SYNC_LOG_FORMAT | `text` | 进程标准输出格式，可选 `json`；SQLite 始终保存结构化字段 |
| SYNC_LOG_PAYLOAD | `off` | 开启后（且等级含 `debug`）才渲染记录/相册内容字段 |
| SYNC_LOG_RETENTION_RAW_SECS | `86400` | 未压缩窗口阈值，范围 1..2592000 |
| SYNC_LOG_ARCHIVE_MAX_BYTES | `1073741824` | 压缩段总量上限（1 GiB），范围 4096..1 TiB |
| SYNC_LOG_QUEUE_CAPACITY | `4096` | 待落库队列容量，范围 16..1048576 |

等级策略：

- 每个请求一条 `request completed`，字段为 `method/route/status/duration_ms/error_code/request_id/user_id`；`/healthz`、`/readyz`、`/v1/metrics` 属 `debug`，`4xx` 属 `warn`，`5xx` 属 `error`，其余 `info`。`user_id` 是请求所属账户（会话解析出的账户上下文）；未认证请求（能力 URL 下载、认证失败）不写该字段。
- 元数据读取/轮询、租约 renew/check/phase/status 属 `debug`；租约获取与释放、资源发布、记录写入、会话签发与撤销、启动恢复、正常停止属 `info`。同步域事件（租约归属、资源发布、记录落定、元数据读取）同样带 `user_id`，使「这个账户做了什么」在请求层与域事件层都可回答。
- 失锁（UPLOAD_LEASE_LOST/UPLOAD_PHASE_MISMATCH）、正文长度或 SHA-256 与预留不符、归档超限裁剪、写库失败属 `warn`/`error`。
- 落库队列满时丢弃**最新**记录并计数，绝不阻塞请求线程。
- 开发环境建议 `SYNC_LOG_LEVEL=debug` 加 `SYNC_LOG_PAYLOAD=on`；生产保持默认 `info` 与 `off`。

## 落库、阈值与压缩

`logs.sqlite`（WAL、`user_version=2`）与元数据数据库分离：日志不进入同步事务，不影响 database generation，也不改变元数据 schema v5。

| 表 | 内容 |
|---|---|
| log_events | 未压缩窗口：`seq/ts_ms/level/target/message/fields(JSON)/request_id/user_id`，STRICT |
| log_segments | 压缩段：`first_seq/last_seq/first_ts_ms/last_ts_ms/row_count/raw_bytes/stored_bytes/codec='gzip'/checksum/created_at_ms/payload`，STRICT |
| log_meta | 生效的保留阈值与归档上限 |

- v1 → v2 迁移在打开时单事务执行（`ALTER TABLE log_events ADD COLUMN user_id` + 索引），既有记录保留、`user_id` 读作 NULL；旧版二进制拒绝打开 v2 库（`SchemaUnsupported`）。

- 达到 `SYNC_LOG_RETENTION_RAW_SECS` 的记录在下一次归档 pass 压缩成 gzip NDJSON 段，并在同一事务内从 log_events 删除，因此一条记录不会同时出现在两处。
- 阈值内的记录（当天）始终原样保留，不裁剪、不设条数或字节上限。
- 只有压缩段受 `SYNC_LOG_ARCHIVE_MAX_BYTES` 约束：超过上限时按 `first_ts_ms` 从最旧段开始整段删除，直到满足上限；被删段不可再读，并留下 `warn` 记录。
- 归档 pass 每 5 秒检查一次，启动后立即执行一次；单批最多 4096 行。仪表计数用增量维护，稳态不做全表统计。
- 记录按批提交（≥512 行或 ≥1 秒），`synchronous=NORMAL`：进程崩溃最多丢失最后一次提交之后的日志，数据库结构不损坏。日志不是同步真值，允许这一有界损失窗口。
- 段的 `checksum` 是 payload 的 SHA-256；读取时校验，损坏的段返回错误而不是静默跳过。gzip member 也可用标准工具直接查看。

## 不进入日志的内容

沿用[安全与运维](../design/security-operations.md)的边界，并由同一渲染路径强制：

- 字段名含 password、passwd、secret、credential、authorization、cookie、apikey、api_key、signing、private_key、token、proof、signature、receipt、capability、session、bearer、url 的值替换为 `[redacted:<字符数>]`，开启 payload 也不还原。
- 字段名含 record_name、relative_path、path、filename、username、title、album、label、zone、desired_key、record_type、body、payload、value、preview、summary、device、digest、hash、checksum、etag 的值，在未开启 payload 时替换为 `[detail-off]`。
- **`user_id` 例外，有意不脱敏**：它是运维按账户排查的唯一手段，只指向账户本身，不指向任何照片。它在 `Collector` 中先于脱敏规则提升为记录字段，因此不受 payload 开关与名单变更影响。同理 `asset_id`（服务端生成的对象 UUID）与 `asset_bytes` 保留原值。能定位到具体某张照片的标识（`record_name`、`relative_path`、`zone` 等）维持脱敏不变。
- 任何单个文本值上限 512 字符，超出部分标注被裁剪的字符数。
- 不写文件路径、SQL、密钥内容、签名 URL、leaseToken、同步 token 或请求正文原文。
- 归档 payload 与未压缩窗口同处一个 0600 文件、0700 私有目录，权限规则与元数据数据库一致。

## 读取

`syncserver logs` 只读打开 `logs.sqlite`，不获取数据目录锁，可在服务运行时执行；它需要 `SYNC_DATA_DIR` 与 `SYNC_SIGNING_KEY_FILE` 配置项存在（后者仅用于配置校验，不读取内容）。

| 选项 | 语义 |
|---|---|
| `--level LEVEL[,...]` | 只输出指定等级，取值 error/warn/info/debug/trace |
| `--since`、`--until` | 相对时长（如 `36h`、`5m`）或 epoch 毫秒 |
| `--contains TEXT` | 消息与字段的不区分大小写子串匹配 |
| `--request-id ID` | 精确匹配某个请求 |
| `--user UUID` | 只输出该账户的记录（须为账户 UUID，畸形即报错） |
| `--limit N` | 最近 N 条，默认 500，上限 100000 |
| `--archive` | 一并读取压缩段 |
| `--json` | 逐行 JSON 输出 |

`GET /v1/admin/logs` 是同一套过滤的管理端形态，供控制台的运行日志页使用。它需要管理员会话，只读打开日志库，不取数据目录锁。

| 参数 | 语义 |
|---|---|
| `level` | 逗号分隔的等级，与 `--level` 相同 |
| `since`、`until` | **仅** epoch 毫秒，不接受 `36h` 这类相对时长 |
| `contains` | 同 `--contains` |
| `requestId` | 同 `--request-id` |
| `user` | 同 `--user`：账户 UUID，畸形即 400 |
| `limit` | 最近 N 条，默认 200，上限 1000（越界即 400） |
| `archive` | `true`/`1`/`false`/`0`，**缺省 true**，即默认一并读取压缩段 |

`since`/`until` 一律是绝对 epoch 毫秒，且必须是可解析的非负整数：一个写错的边界会被拒绝（400），而不是被忽略后返回全量——后者与「搜索确实生效但没有命中」无法区分。该接口的参数是严格白名单，出现未知参数同样返回 400。

响应形如 `{"logs":[…],"persisted":bool,"truncated":bool}`。`persisted=false` 表示该部署未开启落库（`SYNC_LOG_PERSIST_LEVEL=off`），此时 `logs` 为空，避免把「没有落库」显示成「服务器很安静」。`truncated` 表示这一页取满了、可能还有更早的匹配记录；接口**不返回命中总数**，因为统计需要第二次扫描，而带 `contains` 时那是一次无索引的全表扫描，与稳态不做全表统计的约束冲突。每条记录含 `userId`（可为 null，见等级策略）。控制台日志页的按账户汇总在已加载行上客户端计算，同样是这个约束的结果：它描述当前结果集，不承诺账户的全量历史。

`GET /v1/metrics` 的 `metrics.logging` 对象暴露管线健康度，字段见[运维指标](metrics.md)。systemd 与容器配置都不重定向标准输出：进程输出的留存与轮转由 journald 或容器日志驱动负责，服务端不代为配置。Nginx 访问日志保持关闭，因为 capability URL 在查询串中带秘密，见[部署说明](../../deploy/README.md)。

## 验收

| 项 | 场景 | 证据 |
|---|---|---|
| L01 | 低于 persist 等级的记录只到控制台，不落库 | tests/logging.rs 的 `persist_level_filters_records_that_only_reach_the_console` |
| L02 | 凭据字段永远脱敏；库内容字段只在 payload=on 时可见 | tests/logging.rs 的 `secrets_are_always_redacted_and_library_data_needs_detail_logging` |
| L03 | 超过阈值的记录压缩进 gzip 段并可读回；当天记录留在未压缩窗口 | tests/logging.rs 的 `records_older_than_the_threshold_are_compressed_into_segments` |
| L04 | 压缩总量不超过上限，超限时先丢最旧段 | tests/logging.rs 的 `compressed_archive_stays_under_its_cap_and_drops_oldest_segments_first` |
| L05 | 队列满丢最新记录并计数，不阻塞调用线程 | tests/logging.rs 的 `a_full_log_queue_drops_records_instead_of_blocking_the_caller` |
| L06 | 真实进程 dev/prod 两档：等级、detail、错误码、request_id、SIGTERM 前落盘 | tests/logging_process.rs |
| L07 | `logs` 命令读取活库、拒绝畸形参数、缺库明确报错 | tests/logging.rs 的 `logs_command_reads_the_live_database_and_rejects_bad_options` |
| L08 | 非法等级/格式/阈值/队列容量拒绝启动，默认值与继承规则固定 | tests/configuration.rs 的 `logging_configuration_defaults_bounds_and_detail_switch` |
| L09 | `GET /v1/admin/logs` 只允许管理员；过滤参数合法时被接受，写错的边界与未知参数返回 400；未落库时报 `persisted=false`；`requestId` 与 `archive` 能跨压缩段定位同一次请求 | tests/admin_http.rs 的 `the_log_search_validates_its_filters_and_reports_an_unpersisted_deployment`，tests/logging.rs 的 `a_request_can_be_traced_across_the_compressed_boundary` |
| L10 | 认证请求的 `request completed` 与 `session issued` 带同一 `user_id`，未认证请求不归属任何账户；`user_id` 不受 payload 开关影响而 `record_name` 仍脱敏；按账户过滤跨压缩段；v1 库迁移到 v2 且旧记录保留 | tests/logging_process.rs 的 `an_authenticated_request_is_attributed_to_its_account`，tests/logging.rs 的 `an_account_id_survives_without_detail_logging_but_a_record_name_does_not`、`an_account_filter_spans_the_compressed_boundary`、`a_previous_version_database_is_migrated_and_keeps_every_record` |
