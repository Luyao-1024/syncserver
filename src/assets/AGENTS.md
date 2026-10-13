# Assets 模块规则

继承 [src/AGENTS.md](../AGENTS.md)。先读[资源生命周期](../../docs/design/asset-lifecycle.md)、[恢复](../../docs/design/concurrency-recovery.md)、[完整媒体同步](../../docs/design/media-sync.md)。

- 接收字节同一遍计算 SHA-256/大小，校验并稳定发布后才允许引用。
- 路径由可信 ID 派生；排他暂存、符号链接防护、同文件系统发布与 fsync。
- 轮询和 HEAD 只读取资产元数据，不重算 hash。
- 角色是 thumbnail/original；分别限流和验证，避免大原图占尽缩略图请求资源。
- 缩略图生成首版由上传客户端负责，服务端不在请求内转码全部原图。
- 接收会话、幂等和取消都有持久恢复证据。

上传会话/能力/意图绑定 leaseId/fence/intent，失锁取消长流，最终成功确认不得绕过新代次；已持久意图只安全结算/隔离。正文 idle/total 与保活分别计时，读路径不取上行锁。见[协调契约](../../docs/contracts/upload-coordination.md)。
