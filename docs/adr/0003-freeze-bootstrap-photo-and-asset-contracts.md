# ADR 0003：冻结共享快照、照片关系、资源接口与发布屏障

日期：2026-10-13。状态：设计采用，代码验收待后续阶段。

## 决策

- 用 POST /v1/snapshots 创建独立 snapshotToken；不同类型 query/lookup 共享 H。
- 首版 CPLAsset/CPLMaster 一对一；成对创建、内容更新和删除；退休名称不复用。
- 资产 identity 不拥有全局照片 generation；共享 bytes 的角色从受验证属性和绑定核对。
- 固定资源接口、SHA-256 编码、会话状态/期限和签名资源描述。
- atomic 变化可跨页，客户端 inbox 收齐固定 H 后才形成可协调完整投影。
- 客户端以持久索引屏障和本地读取 lease 保护配对发布，不依靠 UI 隐藏。

## 代价与边界

增加快照扩展与客户端本地发布适配；未知 Apple 私有协议仍不支持。服务端、客户端和真实部署验收分别执行；设计闭环不代表测试完成。

决定 zone 配置在已有数据目录内不可静默变化。未来需要 zone 管理、历史裁剪或解除一对一限制时新增 ADR。

关联：[共享快照](../contracts/snapshots.md)、[照片记录](../contracts/photo-records.md)、[资源接口](../contracts/assets.md)、[客户端屏障](../design/client-publication.md)。
