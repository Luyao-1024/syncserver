# Store 模块规则

继承 [src/AGENTS.md](../AGENTS.md)。先读[数据模型](../../docs/design/data-model.md)、[同步状态机](../../docs/design/sync-state-machine.md)、[恢复](../../docs/design/concurrency-recovery.md)。

- 数据库事务同时提交条件、投影、版本、变化和 receipt；不得部分推进。
- 快照分页依赖保留版本和固定水位，不使用当前投影 OFFSET 冒充一致快照。
- 每个查询带 namespace；索引和 EXPLAIN 有实际证据。
- 事务不等待上传、不计算正文 hash、不调用网络。
- 首版不裁剪变化或历史，不暗中删除资源引用。
- published 照片必须同时引用匹配 generation 的已发布原图和缩略图。

租约状态和最后写入在同一所有者的事务中校验，持久 fence 不重置，正常重启不改变同步 generation；lease/续租不分配媒体 seq。读取提交事实无须上行租约。先读[协调契约](../../docs/contracts/upload-coordination.md)。
