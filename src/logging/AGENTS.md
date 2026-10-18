# Logging 模块规则

继承 [src/AGENTS.md](../AGENTS.md)。先读[日志与留存契约](../../docs/contracts/logging.md)、[安全与运维](../../docs/design/security-operations.md)。

- 只有一条 subscriber 与一条脱敏路径：`layer.rs` 渲染控制台并同一记录交给落库，禁止再造第二个过滤或格式化入口。
- 发布日志永不阻塞请求线程：`try_send` 队列满丢最新并计数；日志管道故障只能影响日志本身。
- `activate()` 只在 `Store::open` 取得数据目录独占锁之后调用；`shutdown()` 在进程返回前最后排空，二者顺序不可颠倒。
- 阈值内（当天）日志原样保留且无上限；只有压缩段受归档上限约束，超限时按最旧整段删除并留 `warn`。
- 日志库与元数据库分离：不进同步事务、不改 schema v5、不改 database generation，也不承担同步真值。
- 凭据类字段名永远脱敏，库内容字段仅在 payload 开启时可见；不写文件路径、SQL、token、能力 URL 或请求正文原文。`user_id`/`asset_id`/`asset_bytes` 是运维句柄而非库内容，在 `Collector` 中先于脱敏规则提升，禁止加入任何名单。
- 新增日志站点按等级策略归类：轮询/读取/续租为 debug，状态跃迁为 info，失锁/摘要不符为 warn，写库失败为 error。禁止绕过 `log_at!` 直接拼字符串等级。
- 归档计数用增量维护，稳态不做全表 `COUNT`/`SUM` 扫描；读取路径（`syncserver logs`）只读、不取目录锁。
