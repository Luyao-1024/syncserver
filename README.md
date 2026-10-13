# SyncServer

基于 Rust 设计的独立照片同步服务端，参考 CloudKit 的记录、资源、区域和变化游标模型，为 PhotoViewer 提供增量同步。

**当前阶段：M0 设计契约冻结与仓库初始化。尚无 Rust 服务端实现，也没有可运行的发布版本。** 文档中的接口、配置和性能约束均为待实现契约，不能视为已经通过验证的能力。

## 目标

- 未变化照片的摘要持久化；列举、增量查询和 HEAD 不重新读取正文计算 hash。
- 首次建立一致清单，之后按变化游标同步新增、修改和删除。
- 上传原图与缩略图；下行每张照片的两个资源校验并稳定发布后才进入图库，复用现有本地加载流程。
- 保留客户端的上传范围、下载范围、删除抑制和冲突决策；服务端提供证据和条件写入。
- 兼容已核验的 CloudKit 风格 URL 与 JSON 子集。不是 Apple 服务替代登录，也不宣称完整兼容 iCloud Photos。
- 独立部署；与 `/home/luyao/workspace/photoViewer` 分离，不复制图库数据库或 GTK 依赖。

## 阅读顺序

1. [spac.md：需求、范围与验收](spac.md)
2. [架构与目录](docs/design/architecture.md)
3. [接口兼容范围](docs/design/protocol-compatibility.md)、[共享快照](docs/contracts/snapshots.md)、[照片字段](docs/contracts/photo-records.md)、[资源接口](docs/contracts/assets.md)
4. [数据模型](docs/design/data-model.md)与[同步状态机](docs/design/sync-state-machine.md)
5. [完整媒体同步](docs/design/media-sync.md)、[资源生命周期](docs/design/asset-lifecycle.md)与[事务和恢复](docs/design/concurrency-recovery.md)
6. [安全与部署](docs/design/security-operations.md)
7. [验证计划](docs/design/verification.md)与[分阶段实施](docs/design/implementation-plan.md)
8. [开源参考及复用边界](docs/design/open-source-reference.md)与[初始 ADR](docs/adr/0001-initial-architecture.md)、[完整下载后显示决策](docs/adr/0002-complete-media-before-display.md)、[契约冻结决策](docs/adr/0003-freeze-bootstrap-photo-and-asset-contracts.md)

开发准备度与遗留闭环见[设计闭环检查](docs/design/readiness-review.md)。

Agent 的规范入口是 [AGENTS.md](AGENTS.md)。[Agent.md](Agent.md) 提供各目录规范索引。

## 技术方向

Rust、Tokio、Axum、SQLite WAL、不可变文件对象、SHA-256、签名游标。库版本在创建 Cargo 工程时选择、锁定并验证；现在不提前声明某个组合已构建通过。

首版只覆盖单账户和 `private` 数据库。保留命名空间隔离，后续可扩展账户与授权。首版保留变化历史和资源，不做在线历史裁剪与垃圾回收。

## 阶段边界

M0 为当前文档阶段；M1–M4 为基础服务端；M5 为发布与 PhotoViewer 接入。实现进展必须以代码、测试及部署证据更新，不能用“接口已设计”代替“接口已实现”。

许可证：[MIT](LICENSE)。
