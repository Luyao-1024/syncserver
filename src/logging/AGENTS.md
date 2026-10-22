# Logging 模块规则

继承 [src/AGENTS.md](../AGENTS.md)。先读[日志与留存契约](../../docs/contracts/logging.md)、[安全与运维](../../docs/design/security-operations.md)。

- 只有一条 subscriber 与一条脱敏路径：`layer.rs` 渲染控制台并同一记录交给落库，禁止再造第二个过滤或格式化入口。
- 发布日志永不阻塞请求线程：`try_send` 队列满丢最新并计数；日志管道故障只能影响日志本身。
- `activate()` 只在 `Store::open` 取得数据目录独占锁之后调用；`shutdown()` 在进程返回前最后排空，二者顺序不可颠倒。
- 阈值内（当天）已落库日志原样保留、不按条数裁剪，新增写入受独立物理预算约束；压缩段另受归档上限约束，超限时按最旧整段删除并留 `warn`。
- 日志库与元数据库分离：不进同步事务、不改 schema v5、不改 database generation，也不承担同步真值。
- 凭据类字段名永远脱敏，库内容字段仅在 payload 开启时可见；不写文件路径、SQL、token、能力 URL 或请求正文原文。`user_id`/`asset_id`/`asset_bytes` 是运维句柄而非库内容，在 `Collector` 中先于脱敏规则提升，禁止加入任何名单。
- 新增日志站点按等级策略归类：轮询/读取/续租为 debug，状态跃迁为 info，失锁/摘要不符为 warn，写库失败为 error。禁止绕过 `log_at!` 直接拼字符串等级。
- 归档计数用增量维护，稳态不做全表 `COUNT`/`SUM` 扫描；读取路径（`syncserver logs`）只读、不取目录锁。


## 2026-10-23 公网安全修复：物理容量边界

原始保留窗口内已接收的日志仍不按条数裁剪；物理安全预算通过 `SYNC_LOG_DATABASE_MAX_BYTES`（默认 256 MiB）限制日志 SQLite 增长。预算耗尽时拒绝新增落库、记录 writeFailures，不删除现有近期日志，不阻塞同步事务。归档仍受自身上限约束；部署再以独立受限数据卷限制数据库、WAL 和日志的合计物理占用。此安全预算是本次用户授权修复引入的契约更新，不继续承诺物理磁盘无限增长。
