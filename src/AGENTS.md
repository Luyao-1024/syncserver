# Rust 源码规则

继承根 [AGENTS.md](../AGENTS.md)。当前只有模块规则，尚未创建 Rust 源码。

先读[架构](../docs/design/architecture.md)、[实施计划](../docs/design/implementation-plan.md)。

- 按 api/domain/store/assets/jobs 划分职责，一个 crate 起步，不先拆出无使用者的抽象。
- domain 不依赖 Axum、SQLite 或文件系统；协议适配不直接写 SQL。
- 阻塞数据库、哈希和文件 I/O 不运行在异步请求调度线程上。
- 错误、并发、取消和重启是契约的一部分，禁止用 panic 处理请求错误。
- 修改模块前读取该目录 AGENTS。用户当前要求只做文档，不在此阶段加入功能实现。
