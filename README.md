# SyncServer

独立的 Rust 照片同步服务端，参考 CloudKit 的记录、资源、区域和变化游标模型，为 PhotoViewer 提供增量同步。

**当前阶段：基本服务端已实现到 M4，M5 的 CI 与部署工具已建立。** 支持签名 token、共享固定水位快照、增量变化、单设备上行租约/保活/fencing、原图与缩略图上传、SHA-256 校验/去重、原子照片配对、条件与 Range 下载、幂等状态和故障恢复。参考 [基本实现与验收](docs/design/v0.1-server.md)。PhotoViewer 原生客户端已在相邻仓库实现并验证。当前扩展支持用户名密码和逐账户隔离，见[多用户实现与管理](docs/design/multi-user.md)；本轮本地结果不代表已部署线上多用户。Apple 登录、私有加密/索引及未适配 Photos 不属于兼容范围。

首次正式发布前不要求兼容旧开发版数据库；持久化/恢复和普通重启的 token 连续性继续验证。服务端接口已实现不代表公网部署已完成。

## 已实现接口

| 接口 | 用途 |
|---|---|
| POST /v1/auth/login、/v1/auth/logout | 用户名密码登录、当前会话撤销 |
| GET /healthz、/readyz、/v1/capabilities | 存活、就绪、实际能力/配置/计数 |
| CloudKit zones/list | 固定 zone 集合 |
| records/modify、records/lookup、records/query | 有锁条件写入、当前/快照读取和 keyset 查询 |
| changes/database、changes/zone 与两个旧别名 | 固定 H 增量、完整历史版本、显式墓碑 |
| internal/records/query/batch | 有界基本 query batch，共享事务水位 |
| POST /v1/snapshots | 为多个 recordType 创建共享快照 |
| /v1/upload-leases 及 renew/check/phase/release | 单上传设备、持锁追平、删除→修改→新增 |
| GET /v1/operations/{operationId} | 不取上行锁读取已提交或 Pending 结果 |
| CloudKit assets/upload、/v1/uploads/{id} | 预留、状态、续期和流式上传 |
| /v1/assets/{id} 及 content | 资源描述、GET/HEAD、条件和单段 Range |

CloudKit 基址为 `/database/1/{container}/production/private`。账户模式业务入口使用登录会话 Bearer；上传 content 同时要求会话、签名能力和 lease 三请求头，下载 content 同时要求会话和下载能力。没有用户的旧单账户部署保留原 API key/资源能力接入。资源角色严格 original/thumbnail，不将原图作为缺失缩略图的替代。

## 本地运行

本机验证工具链为 Rust 1.98.0，edition 2024，Cargo 声明最低版本 1.88（最低版本由独立 CI job 构建/测试，具体结果以对应提交为准）。Cargo.lock 已提交；SQLite 通过 bundled 构建。当前使用 Unix 权限和信号接口，在 Linux 上验证；无需 GTK 或相邻 PhotoViewer 工程。

以下创建独立的本地试运行目录和随机签名密钥，不输出密钥。初始化命令使用排他创建，重复执行不会覆盖已有密钥。

```bash
umask 077
export SYNC_LOCAL_DIR="$PWD/local-run"
mkdir -p "$SYNC_LOCAL_DIR"
python - "$SYNC_LOCAL_DIR" <<'PY'
import base64, os, pathlib, secrets, sys
root = pathlib.Path(sys.argv[1])
for name, text in [
    ('signing.key', base64.b64encode(secrets.token_bytes(32)).decode()),
]:
    with os.fdopen(os.open(root / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as stream:
        stream.write(text + '\n')
PY
export SYNC_DATA_DIR="$SYNC_LOCAL_DIR/data"
unset SYNC_API_KEY_FILE
export SYNC_SIGNING_KEY_FILE="$SYNC_LOCAL_DIR/signing.key"
cargo run --locked -- users create alice
cargo run --locked
```

开发阶段建议把日志降到 debug 并打开内容字段：元数据轮询的返回条数与 `moreComing`、变化页数、租约 renew/check/phase/status 会逐条打印，payload 开启后还带上记录操作预览，同步各阶段明细同时进入终端和 `local-run/data/logs.sqlite`。

```bash
export SYNC_LOG_LEVEL=debug SYNC_LOG_PAYLOAD=on
```

留存阈值默认「1 天内的日志原样保存、超过 1 天压缩」，压缩总量默认上限 1 GiB，可用 `SYNC_LOG_RETENTION_RAW_SECS` 与 `SYNC_LOG_ARCHIVE_MAX_BYTES` 调整；当天窗口不设条数或字节上限。生产环境保持 `SYNC_LOG_LEVEL=info` 与 `SYNC_LOG_PAYLOAD=off`，不要长期开启 payload。

查询已落库的日志：`syncserver logs` 只读打开日志库，不取数据目录锁，可在服务运行时执行；`--level`/`--since`/`--until`/`--contains`/`--request-id`/`--limit` 过滤，`--archive` 一并读取压缩段，`--json` 逐行输出。沿用上面的环境变量即可（需要 `SYNC_DATA_DIR` 与 `SYNC_SIGNING_KEY_FILE` 存在，后者只用于配置校验，不读取内容）：

```bash
cargo run --locked -- logs --level debug --since 2h --contains lease
cargo run --locked -- logs --limit 50 --json --archive
```

默认监听 127.0.0.1:8790。保持此终端运行，在另一个终端验证健康状态：

```bash
curl --fail http://127.0.0.1:8790/healthz
curl --fail http://127.0.0.1:8790/readyz
```

在 PhotoViewer 设置中输入本地地址、用户名和密码登录。也可通过以下脚本交互检查，只输出能力，不打印密码或会话：

```bash
python - <<'PYAUTH'
import getpass, json, urllib.request
body = json.dumps({'username': input('Username: '), 'password': getpass.getpass()}).encode()
request = urllib.request.Request('http://127.0.0.1:8790/v1/auth/login', data=body,
    headers={'Content-Type': 'application/json'})
with urllib.request.urlopen(request) as response:
    session = json.load(response)
request = urllib.request.Request('http://127.0.0.1:8790/v1/capabilities',
    headers={'Authorization': 'Bearer ' + session['accessToken']})
with urllib.request.urlopen(request) as response:
    print(json.dumps(json.load(response), indent=2, ensure_ascii=False))
PYAUTH
```

管理命令还包括 `users list`、`users password alice`、`users disable alice`、`users enable alice`。密码默认终端隐藏输入并确认；自动化用 `--password-stdin`，不要把密码放到命令行参数。**原有单账户数据的所有者需显式使用 `users create owner --adopt-legacy` 接管**；创建首个用户后旧 API key 停止授权，其他用户不会继承旧照片。详见[认证契约](docs/contracts/authentication.md)与[升级步骤](docs/design/multi-user.md)。

Ctrl+C 或 SIGTERM 停止接收新操作，排空数据库队列，登记正常停止并释放目录锁。再次使用同一目录和签名密钥启动会保留 databaseGeneration。另一个进程同时使用该目录会得到 DATA_DIR_LOCKED。

## 云端空间配额

每个用户默认拥有 **20 GB（20000000000 字节）** 的资源总配额，原图和缩略图都计入。部署时设置 `SYNC_USER_STORAGE_QUOTA_BYTES=20000000000`，修改后重启服务；所有用户分别享有该上限。单文件原图默认仍为 512 MiB、缩略图为 2 MiB。

使用当前账户登录得到的 Bearer 查询 `GET /v1/storage`（无参数、无须上行租约）：

```json
{"quotaBytes":20000000000,"usedBytes":1000,"reservedBytes":2000,"availableBytes":19999997000,"imageBytes":400,"videoBytes":600,"otherBytes":0}
```

`imageBytes`、`videoBytes` 分别返回图片和视频占用，缩略图计入所属图片或视频；`otherBytes` 为其他/无法识别的占用，三类之和等于 `usedBytes`，不单列缩略图。分类优先使用服务端原始资源 MIME，未知时按虚拟文件路径扩展名判断，仅查询元数据。

用户占用按当前未删除文件的逻辑资源大小计算：不同文件即使引用同一个去重对象，也分别计量；同一文件重复同步不重复计量。原图和缩略图均计入，历史版本及未引用对象不占用户配额。新上传预留逻辑增长，创建/复制/更新记录在事务内最终校验配额，超额返回 HTTP 409 `STORAGE_QUOTA_EXCEEDED`。删除文件立即释放逻辑空间，底层内容对象继续按现有历史保留规则管理。详见[计量与查询契约](docs/contracts/storage-quota.md)。

## 配置与安全

认证库位于 `.authentication/users.sqlite`；普通用户各自使用 `users/<uuid>/metadata.sqlite`、`objects` 和 `staging`，不会跨用户去重。登录会话有效 30 天，服务端仅存摘要；客户端不保存登录密码。数据目录及 objects/staging 必须由服务用户拥有、权限 0700；管理文件与密钥文件为 0600。拒绝符号链接和文件硬链接，不自动放宽文件权限。密钥不得进入 Git。

日志落在同一私有数据目录的 `logs.sqlite`，与元数据库分离：不进同步事务、不改变 schema 或 database generation。凭据类字段（密码、token、签名能力、leaseToken、URL 等）任何配置下都脱敏；recordName、路径、标题、用户名等库内容只在 `SYNC_LOG_PAYLOAD=on` 且等级含 debug 时可见，单个文本值上限 512 字符。1 天内的日志原样保存且不设上限，超过阈值压缩为 gzip 段，压缩总量默认上限 1 GiB、超限时先删最旧段。详见[日志与留存契约](docs/contracts/logging.md)。

默认 container 是 com.apple.photos.cloud，zone 是 PrimarySync。可用 SYNC_ZONES 配置逗号分隔的其他 zone；现有数据目录的 container/zone 集合固定，集合不一致拒绝启动。签名密钥意外更换同样拒绝启动。

HTTP public base URL 只接受 literal loopback IP；非 loopback 绑定必须配置 HTTPS 地址。公网使用 TLS 反向代理；systemd/容器部署配置见 deploy，服务进程不内置 TLS。全部参数、默认值和边界见 [安全与配置](docs/design/security-operations.md)。
日志等级、落库阈值与队列容量同样在启动时校验，非法值拒绝启动，不静默回落到默认值。

## 记录接口

使用 [元数据记录契约](docs/contracts/records.md) 的 create/update/lookup JSON。所有变更先按[上行协调](docs/contracts/upload-coordination.md)获取租约、check 追平，再进入对应阶段，并携带 X-Sync-Lease-ID/Token/Fence。更新 fields 是补丁，null 删除普通字段；更新与删除要求当前 recordChangeTag。HTTP Idempotency-Key 绑定完整批次，在重启/响应丢失后返回既有域结果，nonatomic 中断后只继续尚未完成项。

当前 schema v5 包含事件、租约、资源/会话和发布意图。发布前不承诺开发版数据库兼容；正常重启保留 generation 和范围。通用 STRING/INT64/DOUBLE/TIMESTAMP/BYTES/REFERENCE 及其列表已支持，其他类型明确拒绝；详情及限额见契约。

## 验证

测试覆盖真实 HTTP/SQLite/文件路径、快照和增量分页、token/范围、租约竞争/持锁追平/过期/保活、资源校验/去重/成对发布、条件与 Range、接收事务及实际 SIGKILL/SIGTERM，以及日志脱敏、阈值压缩、归档上限和有界队列。聚焦场景入口：

```bash
cargo test --locked --test users_http --test configuration --test startup_http --test metrics_http
cargo test --locked --test sync_http --test leases_http --test assets_http
cargo test --locked --test records_http --test records_store --test photo_graph --test process_restart
cargo test --locked --lib
python tools/check-docs.py
```

日志管线聚焦验证（脱敏、阈值压缩、归档上限、有界队列、真实进程 dev/prod 两档）：

```bash
cargo test --locked --test logging --test logging_process
```

远端推送前执行 fmt/clippy/build/test 完整闸门，见 [AGENTS.md](AGENTS.md)。容器、只读 smoke、远端 CI 和真实公网部署分别报告，不将其中一项替代其他证据。客户端 dirty 字段、回调、冲突副本、完整媒体入库与 UI 仍属于 C1。

## 设计文档

- [用户认证契约](docs/contracts/authentication.md)、[多用户实现与管理](docs/design/multi-user.md)
- [日志与留存契约](docs/contracts/logging.md)、[运维指标](docs/contracts/metrics.md)、[src/logging 实现规则](src/logging/AGENTS.md)
- [spac.md：需求与验收](spac.md)、[架构](docs/design/architecture.md)、[实施计划](docs/design/implementation-plan.md)
- [M1 基础](docs/design/m1-foundation.md)、[M2 实现](docs/design/m2-records.md)、[元数据记录](docs/contracts/records.md)
- [兼容范围](docs/design/protocol-compatibility.md)、[共享快照](docs/contracts/snapshots.md)、[照片字段](docs/contracts/photo-records.md)、[资源接口](docs/contracts/assets.md)
- [数据模型](docs/design/data-model.md)、[同步状态机](docs/design/sync-state-machine.md)、[事务与恢复](docs/design/concurrency-recovery.md)
- [完整媒体同步](docs/design/media-sync.md)、[资源生命周期](docs/design/asset-lifecycle.md)、[客户端发布屏障](docs/design/client-publication.md)
- [验证计划](docs/design/verification.md)、[设计闭环](docs/design/readiness-review.md)、[开源参考](docs/design/open-source-reference.md)
- [ADR 0001](docs/adr/0001-initial-architecture.md)、[ADR 0002](docs/adr/0002-complete-media-before-display.md)、[ADR 0003](docs/adr/0003-freeze-bootstrap-photo-and-asset-contracts.md)

Agent 入口为 [AGENTS.md](AGENTS.md)，导航为 [Agent.md](Agent.md)。许可证：[MIT](LICENSE)。


本次开发分支约定：SyncServer 继续在 main；PhotoViewer 使用 dev，客户端维护
`docs/modules/native-sync.md` 作为 C1 契约摘要。本次已实现服务端基本能力；PhotoViewer 的 Provider、dirty/回调、完整媒体入库和 UI 接入仍待 C1。

## 基本流程与部署

下行先发现 zone、创建共享快照、query 完整清单，再按 zone token 获取增量；客户端同事务持久 inbox/ingest，完整 H 和范围内资源/冲突就绪后才推进 applied。上行 acquire 失败跳过本轮，持锁 check 落后时继续保活并重新下行；追平后 delete→modify→create，完成后 release。失锁使当前及后续未完成任务失败，客户端 dirty 不清。服务端不能替代客户端数据库或 UI 的实现。

资源传输后 original/thumbnail 都 Published，才原子保存照片对；metadata-only 更新复用资源。上传无续传，响应丢失先查会话/operation 状态。文件强 ETag 由已验证摘要生成，query/changes/HEAD 不重新读取或计算已有文件 hash。

旧单账户/会话文件接入示例：`cargo run --locked --example receive_metadata -- http://127.0.0.1:8790 local-run/api.key local-run/receiver`。第二参数也可为只对用户可读的会话 Bearer 文件，`local-run/api.key` 仅适用于另行配置该文件且没有用户的旧模式。它持久接收元数据与双资源待办，不下载或发布 UI 照片；pending 完成前 applied 保持原位置。服务端真实 HTTP 测试验证其接收/重启/资源确认，但不替代 C1。

首次部署参考 [deploy/README.md](deploy/README.md)。只读检查按接入方式二选一：已有用户的账户部署使用 `python3 tools/smoke.py http://127.0.0.1:8790 --username alice --password-stdin`（口令从标准输入或终端隐藏读取，不进命令行参数），没有用户的旧单账户部署继续使用 `--api-key-file /path/to/api.key`。两者互斥，都不把凭证写入日志。GitHub CI 对 main push/PR 执行完整 Rust 闸门及独立最低版本检查。


运维监控使用受 Bearer 认证的 `GET /v1/metrics`，见[指标契约](docs/contracts/metrics.md)。它不向数据库排队，独立配额在普通/控制请求饱和时仍可采样；包含两个队列、传输角色占用、变化分页与正文成本。计数按本次 runId 重置，不代表客户端已同步完成。部署 smoke 同时验证此入口。


2026-10-15 已完成[阿里云实例部署](docs/design/aliyun-deployment.md)，公网 HTTPS、证书续期模拟、真实双资源与重启验证已通过。部署包含 M5 修改；源码提交与 CI 状态以 Git 交付记录为准，PhotoViewer/C1 尚未接入。
