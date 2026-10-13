# 测试规则

继承根 [AGENTS.md](../AGENTS.md)。先读[验证计划](../docs/design/verification.md)。

- 临时目录、随机端口、合成资产，禁止访问真实相册或 Apple 账户。
- 验证真实 HTTP/事务/文件路径，不直接修改终态制造成功。
- 测 token/快照/条件并发/发布故障/重启，也测正文读取与 hash 成本。
- 协议 fixture 标明官方语义、上游观测或本服务扩展，不声称完整 iCloud 兼容。
- 客户端测试验证双资源入库门槛、缓存导入与恢复，UI 测试留在 PhotoViewer 并遵守其真实输入规则。
- 没有 Cargo 工程时只做文档检查，不冒报接口或运行测试 PASS。

新增协调测试先对照[契约失败矩阵](../docs/contracts/upload-coordination.md)；可控时钟验证过期/续租，不长时间 sleep。客户端 dirty 用真实 DB 字段测试，不能先手动清 dirty 来制造成功；服务端合成客户端不能替代 C1 证据。
