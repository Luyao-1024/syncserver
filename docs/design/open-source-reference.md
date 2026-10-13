# 开源参考、来源与复用边界

调研基线：2026-10-13。本工程尚未引入第三方源码；下面是设计依据与后续复用候选，不代表完成兼容测试。

## 来源

| 项目/资料 | 基线 | 用途 |
|---|---|---|
| [kei](https://github.com/rhoopr/kei) | 源码提交 `30fe9ecab01899e9d1c846958f04cd8b7e526a2d`；Rust、MIT | 实际 iCloud Photos 请求、删除形状、zone/database token、检查点 |
| [kei 的 syncToken 文档](https://github.com/rhoopr/kei/blob/30fe9ecab01899e9d1c846958f04cd8b7e526a2d/docs/synctoken-reference.md) | 固定提交 | 反向工程与观测资料，非 Apple 协议承诺 |
| [Apple CKSyncEngine 示例](https://github.com/apple/sample-cloudkit-sync-engine) | `63d9d16d75429a94d06d77f6dac13cb9a777b09f`；MIT | 客户端本地状态、双向同步和冲突示例 |
| [FoundationDB Record Layer](https://github.com/FoundationDB/fdb-record-layer) | Apache-2.0，Java | 服务端事务、记录版本与索引的架构参考 |
| [Record Layer 论文](https://www.foundationdb.org/files/record-layer-paper.pdf) | 2019 年 | Apple 团队公开的 CloudKit 版本索引与增量同步设计；不断言当前线上部署完全相同 |
| [CloudKit Web Services](https://developer.apple.com/library/archive/documentation/DataManagement/Conceptual/CloudKitWebServicesReference/) | Apple 官方归档资料 | 通用 record、zone、query、modify、asset 接口 |
| [icloud-photos-sync](https://github.com/steilerDev/icloud-photos-sync) | `ca5e7a4cf7123c8194d58c993ef7375a562f9665`；GPL-3.0-or-later | 行为和数据模型交叉核验，不默认复制代码 |
| [icloudpd](https://github.com/icloud-photos-downloader/icloud_photos_downloader) | `879c561240d993d748ddb4546f935090502b16d3`；MIT | 资源解析与下载参考 |
| [SyncKit](https://github.com/mentrena/SyncKit) | `97dcd919d0ab0d9ae903addb5c38cd71fa763ad9`；MIT | Core Data/Realm 与 CloudKit 客户端组织参考 |

这些客户端连接 Apple 服务；它们不是可独立部署的完整 CloudKit/Photos 服务端。Record Layer 是可用底层组件，不包含本项目的全部资源服务和接口。

## 可以复用的内容

优先审查 Rust 的响应模型、分页边界、删除处理和持久检查点。每次复制代码都记录上游提交、文件、许可证和修改范围，并保留要求的版权说明。文档示例应使用合成值，不带真实账户、token 或照片记录。

不要整体复制 Apple 登录、cookie、账户容器选择或私有加密流程；独立服务端拥有自己的认证。不能因为存在相同字段名就认定摘要算法或资产 receipt 兼容。

## 兼容证据分级

- 官方通用接口：作为字段和请求语义依据。
- 开源客户端代码：证明某种实际处理已实现，不证明所有 Apple 请求行为。
- 作者观测/反向工程：作为 fixture 候选，需要独立验证边界。
- 本服务扩展：能力声明明确标识，不能伪装为 Apple 保证。

首版兼容承诺只来自本项目[兼容矩阵](protocol-compatibility.md)及未来测试结果。
