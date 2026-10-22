# 安全、配置与部署

状态：认证/权限/scope、签名读 token、租约与资源能力已实现，部署配置已建立；实际公网部署另行验收。

## 当前身份与隔离

用户名密码、会话撤销、资源双重认证和逐账户存储的当前约束由[认证契约](../contracts/authentication.md)定义。每个账户自己的 SQLite、对象、暂存、签名上下文和租约相互独立；密码以随机盐 Argon2id 保存，会话只存 SHA-256 摘要。认证入口限流并在有界阻塞任务中校验密码。密码和会话不写入日志，文件权限仍由服务 UID 的私有目录保证。实现、CLI 和旧数据接管见[多用户说明](multi-user.md)。

历史单账户模式只在没有用户且配置 API key 时保留。创建首个用户后不再接受该 API key。`shared/public`、开放注册、组织 ACL 和跨账户共享仍不支持。

## token 和 URL

使用经过验证的 HMAC 实现，游标、上传能力、下载能力使用不同域。校验种类、namespace、zone、generation、查询参数、位置、方法和到期时间。

签名 URL 是凭证，不记录完整 query、不进入错误正文。下载 URL 使用配置的 public base URL，不能相信未校验的 Host 或任意代理头。

服务端 SHA-256 证明实际字节身份，不自动证明客户端有权访问另一个 namespace 的资产。

## 配置

| 配置 | 含义 |
|---|---|
| SYNC_BIND | 默认 127.0.0.1:8790，明确配置后才对外暴露 |
| SYNC_DATA_DIR | 必填；数据库、对象、暂存所在根目录，单进程目录锁 |
| SYNC_PUBLIC_BASE_URL | 默认 http://127.0.0.1:8790；HTTP 只允许 literal loopback IP（不接受 DNS 主机名），其他必须 HTTPS；基址不能有用户信息、query/fragment 或路径前缀 |
| SYNC_API_KEY_FILE | 可选；仅无用户旧单账户模式使用的请求认证密钥文件 |
| SYNC_SIGNING_KEY_FILE | 跨重启稳定的签名密钥文件 |
| SYNC_CONTAINER | 默认 com.apple.photos.cloud；已有数据目录不得静默更改 |
| SYNC_ZONES | 部署初始化的 zone 清单，默认 PrimarySync |
| SYNC_USER_STORAGE_QUOTA_BYTES | 每用户当前文件逻辑资源总配额，默认 20000000000（20 GB），正整数且不超过 SQLite 有符号 64 位；重启生效，计量见[配额契约](../contracts/storage-quota.md) |
| SYNC_MAX_ASSET_BYTES | 默认 536870912（512 MiB），可正数配置 |
| SYNC_THUMBNAIL_MAX_BYTES | 默认 2097152，v0.1 不可超过 2 MiB |
| SYNC_UPLOAD_CONCURRENCY | 默认 2，必须正整数 |
| SYNC_DOWNLOAD_CONCURRENCY | 默认 8，必须正整数 |
| SYNC_UPLOAD_IDLE_TIMEOUT_SECS | 默认 30，必须为 u32 范围的正整数 |
| SYNC_UPLOAD_TOTAL_TIMEOUT_SECS | 默认 3600，必须为 u32 范围的正整数 |
| SYNC_DB_QUEUE_CAPACITY | 默认 128，必须正整数 |
| SYNC_LEASE_TTL_SECS | 默认 90，1–86400 秒；heartbeat 必须小于 TTL |
| SYNC_LEASE_HEARTBEAT_SECS | 默认 30，客户端推荐保活周期 |
| SYNC_CONTROL_QUEUE_CAPACITY | 默认 16，独立控制 HTTP/数据库配额 |
| SYNC_JSON_BODY_LIMIT | 默认 1048576（1 MiB），必须正整数 |
| SYNC_LOG_LEVEL | 默认 info，控制台最低等级；`off` 不打印 |
| SYNC_LOG_PERSIST_LEVEL | 默认继承 SYNC_LOG_LEVEL，落库最低等级；`off` 不创建日志库 |
| SYNC_LOG_FORMAT | 默认 text，进程标准输出格式，可选 json |
| SYNC_LOG_PAYLOAD | 默认 off，开启且等级含 debug 时才渲染记录/相册内容字段 |
| SYNC_LOG_RETENTION_RAW_SECS | 默认 86400（1 天），未压缩窗口阈值，1–2592000 |
| SYNC_LOG_ARCHIVE_MAX_BYTES | 默认 1073741824（1 GiB），压缩归档总量上限 |
| SYNC_LOG_QUEUE_CAPACITY | 默认 4096，待落库队列容量，16–1048576 |

数值和 URL 在启动时校验。SYNC_BIND 非 loopback 时 public base 必须为 HTTPS，不能使用默认 HTTP 回环地址充当公开配置。没有密钥、不可写目录或公开 URL 错误时启动失败，不降级为无认证服务。上述 v1 默认值已固定，M1 配置测试和能力声明返回实际配置；变更需同步契约。namespace 与 zone 集合在初始化后固定，顺序调整不算集合变化；不一致按 CONFIG_SCOPE_MISMATCH 拒绝启动。

签名密钥文件为单行标准 Base64，解码恰好 32 随机字节；API key 为单行不透明 ASCII（32–512 字符），建议由 32 随机字节的 Base64URL 生成。Unix 文件仅当前服务用户可读写（0600），不把密钥写入例子。

所有凭证采用版本化、域分离 HMAC-SHA256；payload 的 Base64URL 无 padding。MAC 绑定实际编码 payload 与种类域，校验后再解码检查语义。快照/分页/增量 token 和资产 receipt 首版没有时间到期，受 generation/签名密钥约束；上传/下载能力包含 expires，期限 15 分钟。不得用一种凭证授权另一种操作。

上传 URL 到期只限制新请求；已接受的 Receiving 仍受空闲/总时长上限。已进入发布意图的操作不因凭证到期取消。会话 TTL 24 小时，不回收活跃接收或恢复中的意图。

## 部署

首版单进程、单写入所有者，不允许多进程同时管理同一数据目录。公网入口使用 TLS 反向代理或后续验证的内置 TLS；不能把“支持 Bearer”当成传输加密。

systemd 使用独立用户、受限目录和明确的关闭超时。容器只挂载受管理数据目录及只读密钥。部署样例在 M5 编写，不提前给出不能运行的命令。

## 备份与恢复

运行中元数据备份使用 SQLite 备份机制，不随意复制 WAL 文件。数据备份包含不可变资产、数据库和 generation/签名配置；备份清单验证引用闭包。

恢复备份后显式更新 database_generation，让旧游标重建清单；常规重启不更新。签名密钥轮换会使旧能力失效，需要有明确维护流程。

## 可观察性

日志由单一管线渲染并落库，等级、阈值、脱敏与读取入口见[日志与留存契约](../contracts/logging.md)。每条请求日志带 `request_id`、route、状态、耗时与错误类别；密钥、token、能力 URL 与请求正文原文一律不写，路径和 recordName 等库内容字段默认折叠为 `[detail-off]`，只在显式开启 payload 且等级含 debug 时可见。

健康、就绪、队列长度、上传字节、哈希字节、变化页数和恢复失败计数须可观测。v0.1 不把 metrics 地址当作同步 API。

## M1 的实现边界

M1 的文件权限要求为数据目录 0700、凭证及管理文件 0600，服务用户拥有，拒绝符号链接和文件硬链接。配置文件中的队列/并发/正文限额须能表示为 32 位无符号正整数；资产大小不得超出 SQLite 的有符号 64 位范围，总上传超时不得小于空闲超时。

初始化持久化域分离 HMAC 签名密钥指纹；重启时不一致返回 SIGNING_KEY_MISMATCH，避免意外密钥替换导致未来 token 失效。M1 不提供在线轮换或备份恢复 generation 重置工具；后续工具需独立验证。API key 可在停止服务后更换，不改变数据库 generation。签名密钥与 API key 不允许复用同一内容。

当前配置的上传/下载限额是 M4 的保留配置。M1 已生效的是元数据正文限制、30 秒 JSON 接收上限、数据库队列和受认证请求并发门槛；并发门槛使用 dbQueueCapacity，饱和返回 503，不能将保留配置当成已实现资产传输。

## M2 请求与结果边界

严格 JSON 拒绝重复对象键；Idempotency-Key 拒绝重复头、空白和超长值。修改/当前读取最多 400 项，每记录合并字段 JSON 最多 1 MiB，每响应/事务工作集有 8 MiB 预算。通用引用仅同 zone/action=NONE，不触发外部读取或级联删除。业务冲突允许返回本账户的当前记录；内部错误不泄漏 SQL、磁盘路径或故障注入文本。

## 上行租约新增配置与安全（待实现）

计划增加 SYNC_UPLOAD_LEASE_TTL_SECS（默认 90）和 SYNC_UPLOAD_LEASE_HEARTBEAT_SECS（默认 30，须满足 `0 < heartbeat <= ttl/3`），返回实际配置并暴露 lease/fencing/progress-gate/phase capability。M2 尚无这两个配置或能力，不把计划值当成运行结果。

leaseToken 是秘密，与 Bearer/资源凭证不同域；deviceId 不足以授权。所有变更请求都校验租约/固定范围/阶段，控制路径同样防重复 header 和越权。上传能力绑定租约代次，URL 15 分钟有效并不授予已过期租约写入权。读取和结果状态查询仍只需正常读取认证。

服务器按单调时钟过期，正常重启推进独立持久 fence 并失效租约，保留元数据同步 generation。客户端用成功续租请求的单调起始时刻与已知 TTL 计算保守截止时间，不能把晚到响应接收时刻当成新 90 秒、不能自行延长未确认授权。

已知期限内续租网络错误可以有界重试；服务器明确失锁或保守截止到期令本轮终止，所有未完成项报错。日志只含错误类别/关联 ID，不包含 leaseToken 或上传能力 URL。


## M5 运维指标（2026-10-15 已实现）

受 Bearer 认证的 `GET /v1/metrics` 提供进程计数、两个数据库队列、HTTP/传输占用与启动恢复结果，详见[运维指标](../contracts/metrics.md)。它有独立单请求配额，既不占租约控制配额，也不提交数据库工作；普通/控制 HTTP 配额耗尽或执行器关闭时仍可诊断。启动恢复失败通过退出状态/日志报告，不能从当前进程的恢复报告宣称所有历史故障均可计数。监控字段没有资源 URL、路径、记录或凭证。

部署模板和只读 smoke 已建立，smoke 同时验证指标接口。上述 M1/M2/待实现小节保留阶段历史；当前租约/传输已生效的配置与边界以[基本实现](v0.1-server.md)和[上行协调契约](../contracts/upload-coordination.md)为准。


## 多用户运维补充

同用户的设备共享该用户上行租约；不同用户不竞争同一租约或写入队列。认证目录包含用户 UUID/密码散列/会话摘要，备份须与各用户存储及签名配置共同保留。恢复旧认证备份后应撤销恢复出来的旧会话。管理员账户命令不删除照片，不能把禁用账户当作数据删除。当前实施与验证见[多用户说明](multi-user.md)。


## 2026-10-16：日志落库与留存

`logs.sqlite` 与元数据库分离：未压缩窗口内的日志原样保留且已落库记录不按条数裁剪，新增写入受全库物理预算约束，超过 `SYNC_LOG_RETENTION_RAW_SECS` 的记录压缩成 gzip NDJSON 段，压缩总量受 `SYNC_LOG_ARCHIVE_MAX_BYTES` 约束，超限时先整段删除最旧段并留 `warn`。归档段带 SHA-256 校验和，读取时校验而非静默跳过。

日志库、归档 payload 与数据目录同处 0700 目录/0600 文件的私有权限内，备份须按敏感数据处理，并沿用先停服务再完整备份数据目录的流程。落库队列有界，满时丢最新记录并计数，不阻塞请求线程；进程崩溃最多丢失最后一次提交之后的日志，日志不承担同步真值。

`GET /v1/metrics` 的 `metrics.logging` 只读增量计数器，不打开日志库；`syncserver logs` 只读查询不取数据目录锁，可在服务运行时执行。日志不改变同步 token、快照、租约、资源或 database generation 的语义。开发环境推荐 `SYNC_LOG_LEVEL=debug` 加 `SYNC_LOG_PAYLOAD=on` 以详细记录同步各阶段数据，生产保持默认 `info` 与 `off`。

## 2026-10-23 公网加固

认证、来源限流和角色即时生效见[认证契约](../contracts/authentication.md)；物理预算与低水位见[配额契约](../contracts/storage-quota.md)。默认日志数据库 256 MiB、每账户元数据 1 GiB、认证目录 64 MiB 的物理包络拒绝新增增长，不自动删除已有真值。日志原始窗口保留规则在[日志契约](../contracts/logging.md)同步更新。最终部署必须再限制数据卷与 WAL 的合计空间，并使用专用无登录权限服务用户、ProtectHome、只读程序及最小可写数据路径，避免开放桌面用户整个 home。

管理员临时密码应轮换为唯一随机密码，旧会话随密码重设撤销；2 小时绝对会话期限不能被网页退出时提前清空 Bearer 绕过。公网穿透继续只复用 HTTPS 443，frp 控制及代理端口只监听阿里云回环。具体本次结果见[安全修复实施记录](security-remediation.md)。
