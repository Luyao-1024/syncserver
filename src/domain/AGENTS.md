# Domain 模块规则

继承 [src/AGENTS.md](../AGENTS.md)。先读[数据模型](../../docs/design/data-model.md)、[规格](../../spac.md)、[渐进媒体同步](../../docs/design/progressive-media-sync.md)。

- 用独立类型表达 record/zone/namespace、revision、digest 和游标角色。
- 纯规则负责条件与状态；不接触网络、数据库或磁盘。
- 照片的原图/缩略图属于同一内容 generation；不能把不同版本拼成已发布照片。
- 通用记录与照片领域记录分开验证，不把所有 CloudKit 记录误当照片。
- 服务端不拥有客户端浏览队列、上传范围或删除抑制策略。
