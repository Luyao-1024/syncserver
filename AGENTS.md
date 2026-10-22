# Agent Guide

本仓库是独立的 Rust 照片同步服务端。基本服务端已建立 token/快照/增量、租约、记录、双资源与恢复；具体实现范围以用户最新阶段指令为准。不要把计划写成已实现、不要擅自改动相邻 PhotoViewer 仓库。

## 开始工作

1. 先读 [spac.md](spac.md)、[架构](docs/design/architecture.md)、[实施计划](docs/design/implementation-plan.md)。
2. 按修改路径读取下级 `AGENTS.md`，再读对应设计文档。
3. 文档是待实现契约；发现矛盾时先统一契约。用户已授权的工作不重复要求授权。
4. 用 `rg` 和定向读取检查实际代码。没有 Cargo 工程时不要执行或报告 Cargo 检查通过。

## 本地启动

后续本仓库服务器启动和重启统一使用 `./start.sh dev` 或 `./start.sh release`（省略模式默认 dev）。脚本先编译，成功后优雅停止本仓库 debug/release 旧实例再前台启动；不要另用 `cargo run` 启动服务器。`cargo run -- users ...` / `logs ...` 仍用于管理。配置、默认数据与密钥见 [README](README.md)。systemd/容器部署继续使用自身生命周期管理。

## 必须保留的约束

- 查询变化仅访问元数据；已有照片正文不参与轮询哈希。
- 同步 token、分页 marker、记录 revision 和内容 digest 各有独立语义。
- 清单分页具有固定水位；并发修改不能导致遗漏或读取混合快照。
- 条件写入和记录版本/变化写入处于同一数据库事务。
- 文件系统与 SQLite 不属于一个事务，必须遵守发布与恢复协议。
- SHA-256 由服务端读取实际上传字节验证，不能信任客户端任意声明的 hash。
- 所有路径、游标、资产引用、租约和查询都绑定命名空间。不能把 Apple 私有字段猜测当作官方保证。
- 服务端不替客户端扩大上传范围，也不把本地副本移除自动解释为云端删除。

## 验证与交付

- 开发中优先运行新增或直接受影响的聚焦测试。
- 有 Rust 代码后，远端推送前执行最终代码树的 `cargo fmt --all --check`、`cargo clippy --locked --all-targets -- -D warnings`、`cargo build --locked --all-targets`、`cargo test --locked --all-targets`。
- 仅文档提交执行链接检查、JSON 示例检查和 `git diff --check`；注明 Rust 检查不适用。
- 每个提交都包含 `Tests:`，只记录实际运行的命令和 PASS/FAIL/NOT RUN 结果。
- 报告区分本地文档、已运行代码、已推送提交、远端 CI 和真实部署；不要冒称服务端已经完成。
- 实现改变契约时，同时更新相关设计、兼容矩阵和验收项。

## 新上行协调开发入口

先读[上行协调契约](docs/contracts/upload-coordination.md)与[下一步任务](docs/design/next-development-tasks.md)。服务端已实现 M3/M4，当前行为/证据见 docs/design/v0.1-server.md，客户端 C1 的本地实现与证据见其自身仓库记录。新变更入口必须统一租约/fence/最新进度/阶段，旧 alias 不能绕过；阶段为删除→修改→新增。dirty 为本地既有 DB 字段，禁止独立 dirty 文件/台账/持久队列，失败/跳过/失锁不清，成功回调需检查本地版本/轮次。


## 多用户入口

先读[认证契约](docs/contracts/authentication.md)与[多用户实现](docs/design/multi-user.md)。会话先解析为服务端拥有的账户上下文，再访问记录/资源/租约；不能从请求参数任意选择 namespace。账户密码仅 Argon2id，随机会话仅存摘要，资源须同时验证用户会话及原有能力。用户管理 CLI 不删除照片；旧数据只由唯一显式接管用户拥有。客户端协议仍只引用服务端契约。


## 日志与可观测入口

先读[日志与留存契约](docs/contracts/logging.md)与 [src/logging/AGENTS.md](src/logging/AGENTS.md)。日志单管线脱敏，落 `logs.sqlite`：阈值内已落库日志原样保留、不按条数裁剪，新增写入受独立物理预算约束，超过阈值的记录压缩进归档段，归档总量受上限约束。日志不进同步事务、不改元数据 schema 与 generation；凭据字段永远脱敏，库内容字段只在 payload 开启时可见。新增日志站点按契约的等级策略归类，开发环境用 `SYNC_LOG_LEVEL=debug` 与 `SYNC_LOG_PAYLOAD=on` 获取详细同步数据，生产保持默认。


## 2026-10-23 公网安全修复：物理容量边界

原始保留窗口内已接收的日志仍不按条数裁剪；物理安全预算通过 `SYNC_LOG_DATABASE_MAX_BYTES`（默认 256 MiB）限制日志 SQLite 增长。预算耗尽时拒绝新增落库、记录 writeFailures，不删除现有近期日志，不阻塞同步事务。归档仍受自身上限约束；部署再以独立受限数据卷限制数据库、WAL 和日志的合计物理占用。此安全预算是本次用户授权修复引入的契约更新，不继续承诺物理磁盘无限增长。
