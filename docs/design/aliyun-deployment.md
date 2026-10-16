# 阿里云首次部署与旧 WebDAV 清理

日期：2026-10-15。用户授权通过 `ssh aliyun` 部署 SyncServer，并删除旧 WebDAV 同步数据以释放空间。本记录对应真实服务器操作，不是部署模板或待执行计划。

## 已部署服务

| 项目 | 已核实状态 |
|---|---|
| 公网入口 | `https://47.103.11.237`，HTTPS 证书校验通过 |
| 运行环境 | Ubuntu 24.04.4 LTS，x86_64 |
| 运行方式 | `syncserver.service`，独立 syncserver 用户，active/running、开机启用 |
| 二进制 | `/usr/local/bin/syncserver`，root 拥有、0755 |
| 后端监听 | `127.0.0.1:8790`，由 Nginx 443 转发 |
| 配置 | `/etc/syncserver/server.env` |
| 数据 | `/var/lib/syncserver`，syncserver 拥有、0700 |
| 凭证 | `/etc/syncserver/api.key` 与 `signing.key`，syncserver 拥有、0600，未写入仓库或报告 |
| namespace | container=`com.apple.photos.cloud`、production/private、zone=`PrimarySync` |
| Nginx | `/etc/nginx/sites-available/syncserver`，已启用；使用[仓库模板](../../deploy/nginx.conf.example) |
| 运行检查 | `/usr/local/libexec/syncserver/smoke.py` |
| 构建清单 | `/etc/syncserver/deployment.json`，记录源码/二进制摘要与构建方式 |

复用既有 Let's Encrypt IP 证书 lineage `/etc/letsencrypt/live/webdav-ip`。保留 HTTP ACME webroot，当前证书有效期至 2026-10-19，续期 timer 为 active；真实续期模拟和 deploy hook 成功。活动钩子现只校验并重载 Nginx，旧 WebDAV 重启命令已移除；旧钩子备份移至 `/var/backups/syncserver-transition/20261015T033118Z`，不留在活动钩子目录。

## 构建来源

使用本地 `rust:1.88-bookworm` / `debian:bookworm-slim` 构建镜像中的 release 二进制，在 Ubuntu 上通过 systemd 直接运行；生产服务器不需要 Rust 工具链或容器运行时。

- 基础提交：`2d9f9a74a4168f03f4079537c3336e75cbb0c069`。
- 部署构建时包含尚未提交的本地 M5 运维指标实现；后续源码交付以 Git 记录为准。
- 源码 SHA-256：`91ae1fbee045a339e0e278b4fb5428f1ef6dbd406dd8810eadfbae4cb8db5539`。
- 二进制 SHA-256：`b9b9ee185954cb2a8f6d26f8f7bc37f0b60869ade77f87e5c5a69d5aee5aa8ed`，上传前与服务器安装后相同。
- 本地构建镜像：`localhost/syncserver:aliyun-20261015`。

源码摘要按路径排序，涵盖 Cargo.toml、Cargo.lock、src 下全部 .rs/.sql；依次拼接相对路径、NUL、文件字节、NUL 后计算。服务器清单和本地忽略目录 `local-run/aliyun-deploy` 保存构建标识；这不是已推送新提交或新代码远端 CI 的证据。

## 已授权的数据清理

核实 rclone 的真实服务根目录是 `/srv/webdav`，它不是符号链接或独立挂载，内部没有符号链接或嵌套挂载。停止并禁用 `webdav.service` 后，仅清理该目录内容，保留目录及原有小型配置；没有删除其他服务数据。

- 删除文件：3,544 个。
- 逻辑总大小：20,606,793,593 字节，约 19.2 GiB。
- 已分配总大小：20,614,037,504 字节。
- 清理后 `/srv/webdav` 剩余条目：0。
- 根磁盘：使用率从 74% 降到 22%，可用空间从约 10 GiB 增到约 30 GiB。
- WebDAV 最终状态：inactive / disabled；443 由 Nginx 提供 SyncServer。

旧数据已删除，没有执行 WebDAV 到原生 SyncServer 的数据迁移。PhotoViewer 的本地照片未修改；客户端 C1 仍未接入，原 WebDAV 任务不能直接使用这个原生协议入口。

## 实机验证

| Tests: 实际执行的检查 | 结果 |
|---|---|
| `podman build --pull=never -f deploy/Dockerfile -t localhost/syncserver:aliyun-20261015 .` | PASS，Rust 1.88.0 release 构建 |
| 服务器 `systemd-analyze verify /etc/systemd/system/syncserver.service` | PASS |
| 服务器 `nginx -t` 与 reload | PASS |
| 本地及服务器通过公网 HTTPS 执行 `tools/smoke.py` | PASS，健康/就绪/认证能力/监控/快照/变化 |
| 公网合成双资源验收 | PASS，真实 HTTP/1.1 分块上传、SHA-256、会话 Published、GET/HEAD/Range/304 |
| 公网租约与照片对验收 | PASS，租约竞争拒绝、阶段转换、atomic 成对创建/lookup/条件成对删除 |
| 公网空变化及 HEAD 成本 | PASS，连续 5 轮没有增加正文读取/哈希计数 |
| `systemctl restart syncserver.service` 后检查 | PASS，generation 不变、旧同步 token 继续有效、runId 改变 |
| `certbot renew --cert-name webdav-ip --dry-run --run-deploy-hooks` | PASS，续期模拟和实际 deploy hook 成功 |
| 运行中 SQLite 只读 `quick_check` / `foreign_key_check` | PASS，ok / 0 条失败 |
| 最终数据及权限清点 | PASS，0 条活跃记录、暂存目录为空、两个合成对象共 140 字节，凭证 0600 |

双资源测试的照片记录已成对删除。首版没有资产 GC，因此保留两份共 140 字节的合成对象及少量会话/墓碑历史；没有导入用户照片。

上一步 M5 实现的最终本地 Rust 闸门已通过，63 个测试；本次部署没有修改 Rust 源码。初次部署验收时尚未提交或推送 Git，新代码的远端 CI 当时未运行；后续提交、推送与 CI 以 Git 交付记录为准。没有重启整台服务器，也没有将本次首次部署当作大规模性能、备份恢复演练或 C1 的验收。

## 连接与日常检查

用户机器上连接说明保存在 `/home/luyao/.config/syncserver/aliyun/connection.json`，API key 保存在同目录 `api.key`；目录 0700、文件 0600。签名私钥保留在服务器，不下发客户端。

```sh
python tools/smoke.py https://47.103.11.237 --api-key-file /home/luyao/.config/syncserver/aliyun/api.key
ssh aliyun 'sudo systemctl status syncserver.service --no-pager'
ssh aliyun 'sudo journalctl -u syncserver.service -n 50 --no-pager'
```

后续仍需补齐自动化备份恢复工具、3 万记录基准及真实客户端接入；修改签名密钥或恢复旧数据库前遵守[运维约束](security-operations.md)。

## 2026-10-16 升级到多用户与日志留存版本

用户要求把最新工程部署到阿里云测试服务器，并先提交到 GitHub。本次部署对象是本地 `main` 的三个未推送提交：`6e66cb3`（多用户认证与按用户隔离存储）、`d8182c6`（日志落库与压缩归档）、`935bf3b`（只读冒烟改用会话登录）。部署期间服务器上有真实客户端在轮询，因此只允许只读核对与既有数据延续检查，没有做任何数据迁移或实验。

### 构建与安装

| 项目 | 值 |
|---|---|
| 构建镜像 | `localhost/syncserver:aliyun-20261016`，image id `dd5ff574b52748f0ac4ee83e6c9d2b51cd6ae9a29c225d0bc3f67f531ce73f81` |
| 工具链 | `rust:1.88-bookworm` 构建、`debian:bookworm-slim` 运行，Rust 1.88.0，`release` profile |
| 源码 SHA-256 | `b75779174a0dd98821bab60216a83721aab936ff16806c839a12af923fabce3d`（47 个文件，按[上文摘要方法](#构建来源)） |
| 二进制 SHA-256 | `bb3f8b48251eb3446ff23daab231fe1a8576744e598cad4a0c64e06dd465d390`，上传前后与安装后相同 |
| 被替换的二进制 | `b6e2d346393d2747dc26a388b7d188bf51021922542dc6e659e734df8ddb9797` |
| 停机窗口 | `06:36:52Z` 停止 → `06:36:58Z` active，约 6 秒；验证用第二次 restart 约 4 秒 |
| 升级前备份 | `/var/backups/syncserver-upgrade-20261016T063652Z`，含整份 `/var/lib/syncserver`、`/etc/syncserver` 与旧二进制 |

备份与在线目录同为 349 个文件、376,487,258 字节；`metadata.sqlite`、`.authentication/users.sqlite` 和 `users/504cd9ee-…/metadata.sqlite` 三份副本 `PRAGMA quick_check` 均为 ok。停止服务后 WAL 已 checkpoint，因此文件级拷贝是一致的。

`/etc/syncserver/deployment.json` 之前仍写着首次部署的 `baseCommit 2d9f9a7` 与 `b9b9ee18…` 二进制摘要，与实际运行的多用户二进制不符；本次改为 `baseCommit 935bf3b`、`includesLocalChanges false` 和上表真实摘要。

### 生效的日志配置

`/etc/syncserver/server.env` 只追加了日志键，其余键未改动：

```
SYNC_LOG_LEVEL=debug
SYNC_LOG_PERSIST_LEVEL=info
SYNC_LOG_FORMAT=json
SYNC_LOG_PAYLOAD=on
SYNC_LOG_RETENTION_RAW_SECS=86400
SYNC_LOG_ARCHIVE_MAX_BYTES=1073741824
SYNC_LOG_QUEUE_CAPACITY=4096
```

这是开发阶段的测试服务器，因此控制台与 journald 打印 `debug`（轮询、租约续期、元数据读取返回条数等），并允许 payload 字段；落库仍保持 `info`，使当天原始窗口只包含状态跃迁与请求结果，不被轮询噪声填满。`SYNC_DATA_DIR` 未变，`logs.sqlite` 落在同一 0700 数据目录内，systemd 沙箱无需新增写路径。按[日志与留存契约](../contracts/logging.md)，正式生产应回落为 `SYNC_LOG_LEVEL=info`、`SYNC_LOG_PAYLOAD=off`。

### 认证状态变化

出现账户后 legacy API key 不再授权（`legacy_allowed` 要求“无任何账户”），`api.key` 访问 `/v1/capabilities` 现在返回 401：

| 账户 | 用户 ID | 归属 | 说明 |
|---|---|---|---|
| `luyao` | `c5980085-ffca-460b-9d0a-392ca5483e53` | `legacy=1`，已接管根命名空间 | 现网客户端使用其会话，升级期间会话与数据未变 |
| `tester` | `504cd9ee-fe6e-4125-b962-106a2c95d6f8` | `legacy=0`，独立命名空间 | 首次多用户验证遗留，账号内 0 个对象 |
| `deploycheck` | `d8fb9cb7-7c19-45a8-b4b3-ba9ec6d2a3a4` | `legacy=0`，独立命名空间 | 本次为只读冒烟新建，口令只存在本地 `~/.config/syncserver/aliyun/deploycheck.pass`（0600） |

账号口令没有写入仓库、文档或服务器外的其他位置；口令通过 `--password-stdin` 传入 CLI，未经命令行参数。原 `api.key` 保留但已不能用于账号化部署的读路径。

### 实机验证

| Tests: 实际执行的检查 | 结果 |
|---|---|
| `podman build --pull=never -f deploy/Dockerfile -t localhost/syncserver:aliyun-20261016 .` | PASS，release 构建 |
| 上传前后与安装后 `sha256sum` 比对 | PASS，三处一致 |
| 备份文件数/字节数与 `PRAGMA quick_check`（3 个库） | PASS，349 文件 / 376,487,258 字节 / 全部 ok |
| 公网 HTTPS `python3 tools/smoke.py … --username deploycheck --password-stdin` | PASS，健康/就绪/能力/指标/快照/增量 |
| 升级前后只读对比 `server_meta`、`records`、`assets`、`photo_bindings`、`zones`、`changes` | PASS，schema v5 与 generation `13e1db63-5de1-40ea-ac2f-1d33c39bddcd` 不变，计数 346/344/172/1/350 不变 |
| `systemctl restart syncserver.service` 后延续检查 | PASS，日志库行数 193 → 198 且最旧 `ts_ms` 不变，generation 不变 |
| journald 新格式 | PASS，NDJSON，含 `level/target/fields/request_id/ts_ms`，`debug` 可见 `metadata_operation=changes/zone returned_records=0` |
| `GET /v1/metrics` 的 `metrics.logging` | PASS，`activated true`、`persistLevel info`、`archiveMaxBytes 1073741824`、`queueDepth 0`、`droppedQueueFull 0`、`writeFailures 0` |
| `syncserver logs --limit/--contains/--json/--archive/--level debug` | PASS，只读；`--level debug` 为空，与 `SYNC_LOG_PERSIST_LEVEL=info` 一致 |
| 库内脱敏抽查（`log_events.fields` 键集合与 `target` 去重） | PASS，仅 `status/duration_ms/method/route/request_id/lease_operation/error_code/expires_at/user_id/username/…`，无口令、token、Authorization 值 |
| 重启后请求状态分布（`journalctl` 聚合） | PASS，200×194、404×7（公网扫描器）、401×1（本地未鉴权探测自身），无 409/410 |
| 同二进制 + 独立临时数据目录的压缩链路验证（`retention 1s`） | PASS，22 行归档，段内 5855 → 1141 字节；`archiveBytes 1520 ≤ 4096` |
| 同上，加大流量触发归档上限 | PASS，410 行时 `archiveBytes 655 ≤ cap 4096`、`archiveSegments 3`、`archivedRows 409`、`trimmedSegments 2` |
| 非法 `SYNC_LOG_ARCHIVE_MAX_BYTES=1200` | PASS，启动即 `CONFIG_INVALID: compressed log archive must be between 4 KiB and 1 TiB` |
| 本地最终代码树闸门 | 见本次提交 `Tests:` 行 |

压缩上限验证使用服务器上同一二进制、`127.0.0.1:8791` 的 transient 单元和 `/tmp` 独立数据目录，未接触生产数据目录与端口；验证后单元已删除、临时目录已清理。

### 未覆盖与后续风险

- `certbot.timer` 当前 inactive，证书 `notAfter=Oct 22 13:29:56 2026 GMT`，约 6 天内到期，需要人工确认续期方式。
- 服务器 `/tmp/metadata.sqlite`、`-shm`、`-wal` 是 2026-10-15 17:13 留下的 root 0644 拷贝，含记录数据，属上次排查遗留；本次未删除，建议清理。
- 仍未实现自动备份恢复工具、3 万记录基准与真实客户端（C1）验收；本次也没有做长时间稳定性观察。
- 三个代码提交此前仅在本地；推送与推送后的远端 CI 结果以 Git 交付记录和 Actions 运行状态为准，本次部署不等同于远端 CI 通过。
