# 测试规则

继承根 [AGENTS.md](../AGENTS.md)。先读[验证计划](../docs/design/verification.md)。

- 临时目录、随机端口、合成资产，禁止访问真实相册或 Apple 账户。
- 验证真实 HTTP/事务/文件路径，不直接修改终态制造成功。
- 测 token/快照/条件并发/发布故障/重启，也测正文读取与 hash 成本。
- 协议 fixture 标明官方语义、上游观测或本服务扩展，不声称完整 iCloud 兼容。
- 客户端分层调度测试验证请求顺序和去重，UI 测试留在 PhotoViewer 并遵守其真实输入规则。
- 没有 Cargo 工程时只做文档检查，不冒报接口或运行测试 PASS。
