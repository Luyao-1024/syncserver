# Jobs 模块规则

继承 [src/AGENTS.md](../AGENTS.md)。先读[恢复](../../docs/design/concurrency-recovery.md)、[资源](../../docs/design/asset-lifecycle.md)。

- 启动恢复完成后才 ready；健康存活不等于可安全处理写入。
- 工作队列与重试有界；关闭时结算已发出的安全操作。
- 只回收过期未发布会话，首版不做资产 GC 或历史裁剪。
- 只校验恢复意图涉及的对象，不每次启动重新哈希全部资产。
- 用户浏览原图优先级属于客户端；服务端提供有界资源服务和可恢复下载。
