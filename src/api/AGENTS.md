# API 模块规则

继承 [src/AGENTS.md](../AGENTS.md)。先读[兼容矩阵](../../docs/design/protocol-compatibility.md)、[安全](../../docs/design/security-operations.md)。

- 拥有 URL/JSON、认证、范围、请求限制与错误适配；不持有 SQL 或正文 hash。
- text/plain JSON、zones 数组、墓碑和逐项错误必须走实际 HTTP fixture。
- 不支持的查询/操作明确拒绝，不忽略其语义来返回成功。
- token 和资源 URL 绑定范围与方法；不得信任 Host 生成外部 URL。
- 兼容别名共享领域逻辑，不能各自实现出不同版本控制。
- API 提供角色分明的缩略图/原图描述；客户端成对下载并入库，UI 不承担网络下载。
