# Agent 文档索引

规则的唯一规范入口是 [AGENTS.md](AGENTS.md)；本文件仅满足工程导航，不重复定义约束。

| 工作范围 | 阅读 |
|---|---|
| 全仓库 | [根 AGENTS](AGENTS.md)与[spac.md](spac.md) |
| Rust 模块 | [src/AGENTS.md](src/AGENTS.md) |
| 接口和兼容适配 | [src/api/AGENTS.md](src/api/AGENTS.md) |
| 领域类型与纯规则 | [src/domain/AGENTS.md](src/domain/AGENTS.md) |
| 持久化、游标与事务 | [src/store/AGENTS.md](src/store/AGENTS.md) |
| 上传、对象和下载 | [src/assets/AGENTS.md](src/assets/AGENTS.md) |
| 后台恢复与调度 | [src/jobs/AGENTS.md](src/jobs/AGENTS.md) |
| 测试 | [tests/AGENTS.md](tests/AGENTS.md) |
| 设计文档 | [docs/AGENTS.md](docs/AGENTS.md) |
| 部署 | [deploy/AGENTS.md](deploy/AGENTS.md) |

这些规则不自动授权创建并行 Agent；是否委派以用户和当前工具规则为准。
