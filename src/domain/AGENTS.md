# Domain 模块规则

继承 [src/AGENTS.md](../AGENTS.md)。先读[数据模型](../../docs/design/data-model.md)、[规格](../../spac.md)、[完整媒体同步](../../docs/design/media-sync.md)。

- 用独立类型表达 record/zone/namespace、revision、digest 和游标角色。
- 纯规则负责条件与状态；不接触网络、数据库或磁盘。
- 照片的原图/缩略图属于同一内容 generation；不能把不同版本拼成已发布照片。
- 通用记录与照片领域记录分开验证，不把所有 CloudKit 记录误当照片。
- 服务端不拥有客户端加载队列、上传范围或删除抑制策略；双资源就绪后才可见。

租约/期限/fencing/固定 scope 和 Delete→Modify→Create 规则以[协调契约](../../docs/contracts/upload-coordination.md)为准；token 比较按 kind/scope/generation/完整水位语义，不能按字节相同。纯图仅依赖稳定资源身份，暂时租约/receipt 不进入图或逻辑幂等指纹。
