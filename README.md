# SyncServer

独立的 Rust 照片同步服务端，参考 CloudKit 的记录、资源、区域和变化游标模型，为 PhotoViewer 提供增量同步。

**当前阶段：M2 记录事务与持久化已实现并通过本地测试。** 支持通用元数据记录的创建、当前读取、条件更新/删除、历史版本、变化索引和幂等事务。跨请求快照/增量接口仍待 M3；可信原图/缩略图上传与照片正向发布待 M4。实现与证据见 [M2 文档](docs/design/m2-records.md)。

## 已实现接口

| 接口 | 认证 | 用途 |
|---|---|---|
| GET /healthz | 无 | 进程存活 |
| GET /readyz | 无 | 启动检查完成且数据库可用 |
| GET /v1/capabilities | Bearer | 实际配置、能力支持位、generation、恢复和队列计数 |
| POST /database/1/{container}/production/private/zones/list | Bearer | 列出已配置 zone，接受 JSON/text/plain 空对象 |
| POST /database/1/{container}/production/private/records/modify | Bearer | 通用记录 create/update/delete、atomic/nonatomic 和幂等 |
| POST /database/1/{container}/production/private/records/lookup | Bearer | 批量当前读取、逐项错误和 desiredKeys |

能力声明 zoneListing/recordCrud 为 true；snapshots/incrementalChanges/assetUpload/assetDownload/photoPublication 等为 false。照片关联规则已校验，但 ASSETID 写入尚无可信资源来源，明确拒绝。已知但未实现的接口返回 501 UNSUPPORTED_CAPABILITY，不返回假成功。

## 本地运行

本机验证工具链为 Rust 1.98.0，edition 2024，Cargo 声明最低版本 1.88（未在最低版本上运行验证）。Cargo.lock 已提交；SQLite 通过 bundled 构建。当前使用 Unix 权限和信号接口，在 Linux 上验证；无需 GTK 或相邻 PhotoViewer 工程。

以下创建独立的本地试运行目录和随机凭证，不输出密钥。初始化命令使用排他创建，重复执行不会覆盖已有密钥。

```bash
umask 077
export SYNC_LOCAL_DIR="$PWD/local-run"
mkdir -p "$SYNC_LOCAL_DIR"
python - "$SYNC_LOCAL_DIR" <<'PY'
import base64, os, pathlib, secrets, sys
root = pathlib.Path(sys.argv[1])
for name, text in [
    ('api.key', secrets.token_urlsafe(32)),
    ('signing.key', base64.b64encode(secrets.token_bytes(32)).decode()),
]:
    with os.fdopen(os.open(root / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as stream:
        stream.write(text + '\n')
PY
export SYNC_DATA_DIR="$SYNC_LOCAL_DIR/data"
export SYNC_API_KEY_FILE="$SYNC_LOCAL_DIR/api.key"
export SYNC_SIGNING_KEY_FILE="$SYNC_LOCAL_DIR/signing.key"
cargo run --locked
```

默认监听 127.0.0.1:8790。保持此终端运行，在另一个终端验证健康状态：

```bash
curl --fail http://127.0.0.1:8790/healthz
curl --fail http://127.0.0.1:8790/readyz
```

认证检查可通过 Python 读取本地凭证，避免把密钥展开到进程命令行：

```bash
python - <<'PY'
import json, pathlib, urllib.request
key = pathlib.Path('local-run/api.key').read_text().strip()
request = urllib.request.Request('http://127.0.0.1:8790/v1/capabilities',
    headers={'Authorization': 'Bearer ' + key})
with urllib.request.urlopen(request) as response:
    print(json.dumps(json.load(response), indent=2, ensure_ascii=False))
PY
```

Ctrl+C 或 SIGTERM 停止接收新操作，排空数据库队列，登记正常停止并释放目录锁。再次使用同一目录和签名密钥启动会保留 databaseGeneration。另一个进程同时使用该目录会得到 DATA_DIR_LOCKED。

## 配置与安全

数据目录及 objects/staging 必须由服务用户拥有、权限 0700；管理文件与密钥文件为 0600。拒绝符号链接和文件硬链接，不自动放宽文件权限。密钥不得进入 Git。

默认 container 是 com.apple.photos.cloud，zone 是 PrimarySync。可用 SYNC_ZONES 配置逗号分隔的其他 zone；现有数据目录的 container/zone 集合固定，集合不一致拒绝启动。签名密钥意外更换同样拒绝启动。

HTTP public base URL 只接受 literal loopback IP；非 loopback 绑定必须配置 HTTPS 地址。公网使用 TLS 反向代理，内置 HTTPS 与部署样例待 M5。全部参数、默认值和边界见 [安全与配置](docs/design/security-operations.md)。

## 记录接口

使用 [元数据记录契约](docs/contracts/records.md) 的 create/update/lookup JSON。更新 fields 是补丁，null 删除普通字段；更新与删除要求当前 recordChangeTag。HTTP Idempotency-Key 绑定完整批次，在重启/响应丢失后返回既有域结果，nonatomic 中断后只继续尚未完成项。

从 M1 数据目录启动会自动迁移到 schema v2，保留 generation 和范围。通用 STRING/INT64/DOUBLE/TIMESTAMP/BYTES/REFERENCE 及其列表已支持，其他类型明确拒绝；详情及限额见契约。

## 验证

当前共 35 个测试，包括 M1 的启动/权限/认证及 M2 的真实 HTTP 事务、照片图规则、迁移、SQL 故障和执行中 SIGKILL 恢复。M2 聚焦运行：

```bash
cargo test --locked --test records_http --test records_store --test photo_graph --test process_restart
cargo test --locked --lib store::migrations
python tools/check-docs.py
```

远端推送前执行 fmt/clippy/build/test 完整闸门，详见 [AGENTS.md](AGENTS.md)。完整同步性能、资产恢复、远端 CI、真实公网部署和 PhotoViewer 接入尚未验证。

## 后续目标

- 持久化已验证内容摘要；列举、增量查询和 HEAD 不重新读取既有正文计算 hash。
- 首次建立一致清单，之后按变化游标同步新增、修改和删除。
- 上传原图与缩略图；每张照片的两个资源校验并稳定发布后才进入客户端图库，复用现有本地加载流程。
- 保留客户端上传范围、下载范围、删除抑制和冲突决策，服务端提供证据和条件写入。
- 兼容已核验的 CloudKit 风格 URL 与 JSON 子集；不提供 Apple 登录或原生 Photos 客户端登录，不宣称完整 iCloud 兼容。

M2 已完成元数据记录、revision、历史版本与事务；下一阶段 M3 完成共享快照与增量；M4 完成双资源接收、稳定发布与恢复；M5 负责发布和部署；C1 单独接入 PhotoViewer。

## 设计文档

- [spac.md：需求与验收](spac.md)、[架构](docs/design/architecture.md)、[实施计划](docs/design/implementation-plan.md)
- [M1 基础](docs/design/m1-foundation.md)、[M2 实现](docs/design/m2-records.md)、[元数据记录](docs/contracts/records.md)
- [兼容范围](docs/design/protocol-compatibility.md)、[共享快照](docs/contracts/snapshots.md)、[照片字段](docs/contracts/photo-records.md)、[资源接口](docs/contracts/assets.md)
- [数据模型](docs/design/data-model.md)、[同步状态机](docs/design/sync-state-machine.md)、[事务与恢复](docs/design/concurrency-recovery.md)
- [完整媒体同步](docs/design/media-sync.md)、[资源生命周期](docs/design/asset-lifecycle.md)、[客户端发布屏障](docs/design/client-publication.md)
- [验证计划](docs/design/verification.md)、[设计闭环](docs/design/readiness-review.md)、[开源参考](docs/design/open-source-reference.md)
- [ADR 0001](docs/adr/0001-initial-architecture.md)、[ADR 0002](docs/adr/0002-complete-media-before-display.md)、[ADR 0003](docs/adr/0003-freeze-bootstrap-photo-and-asset-contracts.md)

Agent 入口为 [AGENTS.md](AGENTS.md)，导航为 [Agent.md](Agent.md)。许可证：[MIT](LICENSE)。
