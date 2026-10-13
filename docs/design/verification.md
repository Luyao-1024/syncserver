# 验证计划

状态：M1/M2 测试已加入；完整同步场景待后续阶段。M1 执行证据见 [M1 基础](m1-foundation.md)。对应[spac.md 的 A01–A23](../../spac.md)。

## 文档阶段

- 检查全部本地 Markdown 相对链接指向已创建文档；未来代码路径写成普通代码文本。
- 解析所有 JSON fenced examples，避免伪 JSON。
- 检查根与分层 AGENTS 入口、首版范围和兼容矩阵一致。
- 执行 `python tools/check-docs.py --docs-only` 校验链接、JSON、合成 PNG 摘要和畸形引用样例；这不是服务端测试。
- 执行 git diff --check。
- 不运行也不报告 Cargo 构建、接口测试、部署测试或远端 Rust CI 成功。

## 测试层次

| 层次 | 必须覆盖 |
|---|---|
| domain 单元 | 条件规则、revision 与 digest 区分、状态转换 |
| store 集成 | 事务回滚、版本与变化一致、固定水位、SQL 查询计划 |
| protocol fixture | zones 数组、逐项错误、墓碑、query marker、text/plain JSON |
| asset 文件 I/O | SHA-256、去重、流式边界、Range、路径安全 |
| 服务进程 | 真正 HTTP 请求、持久目录、重启与取消 |
| 故障注入 | 发布每个窗口、响应丢失、磁盘不足、事务失败 |
| 客户端接收示例 | 清单/待办/token 同事务，传输失败不丢工作 |

临时目录与随机端口隔离；不得写入真实相册、Apple 账户或生产数据。使用真实事务/文件/HTTP 路径验证可靠性，不能直接改数据库终态来制造成功。

## 关键场景

1. 首次查询分页过程中插入、修改、删除，并在最终 H 后查询变化；验证没有遗漏或混合快照。
2. recordType 变更被拒绝；删除后查询 H/当前视图，验证先解析最后版本再过滤，不把旧类型行或墓碑当活跃记录。
3. 多 zone 共用 database 页，重复 zone 合并；验证 global/zone token 不可互换。
4. 条件更新与删除并发，计数恰好一个提交和一条变化。
5. atomic 批次后项失败，所有版本/变化/receipt 一起回滚。
6. 上传完成后改变同一会话请求；验证幂等冲突而非返回无关成功。
7. 上传发布各步骤杀进程并重启；验证只恢复有关对象，没有全库哈希。
8. 资源描述中的签名 URL 到期后重取，记录 revision 与变化数不增加。
9. HEAD、空变化查询、重复空变化查询，asset read/hash 计数保持不变。
10. token 篡改、跨 scope、未来位置、备份恢复 generation 错误和服务端 SQL 故障使用不同错误。
11. 照片成对发布，混合 generation 或缺缩略图失败；只改收藏复用两角色。
12. 缩略图 URL 只返回缩略图，不触发原图读取；缩略图超限/格式错被拒绝。
13. 两个 recordType 共享 S/H，查询间更新两记录，下一 delta 必须补齐；snapshot lookup 与跨页 atomic inbox 恢复。
14. 成对删除、非 atomic 拆对、退休名称、重复路径、共享字节与 contentGeneration 复用。
15. 单会话并发、重复 Published 正文、renew/过期/断线、新描述符和幂等旧结果刷新。
16. 固定 zone 配置重启不一致拒绝，不能因空 zone 未产生记录事件而遗漏范围。

后续客户端另测任一资源未完成时新照片不可见、双资源完整后逐张入库、缓存导入命中、点击使用本地原图、watcher/scanner 不绕过门槛、更新失败保留旧完整版本，以及配对发布各窗口的重启恢复。UI 必须在 PhotoViewer 通过真实输入和网络阻塞夹具验证，本仓库服务端测试不代替 UI 证据。

## 性能证据

分别测量首次导入、首次清单、稳态无变化、单记录变化、100 条变化、查询分页、重启恢复。记录正文读取/哈希字节、数据库成本、网络字节和页数。

30,000 条记录及多历史版本作为基准，生成测试资产而不是访问私人照片。报告硬件和工作集、冷/热缓存、页大小、并发与样本分布；耗时用于比较，不替代正确性。

## 未来 Rust 闸门

Cargo 工程存在后，在最终代码树执行：

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --all-targets
cargo test --locked --all-targets
```

随后推送并等待该 SHA 的远端 CI 终态。只有实际发布和 smoke test 通过后，才能更新 README 为可运行版本。性能通过与公网部署是独立证据。

## M1 聚焦验证

新增 configuration、startup_http、persistence、process_restart 测试与 store 队列单元测试。使用合成凭证、临时目录、随机 loopback 端口；验证真实 HTTP、真实 SQLite、真实子进程信号退出。M1 已有 Cargo 工程，文档检查使用 `python tools/check-docs.py`，不能再使用拒绝 Rust 文件的 --docs-only。

M1 的正文计数为 0 是尚无资产操作的事实，未满足 A01 的上传后百次增量轮询验收。该验收仍等待 M3/M4，不能从基础测试推导出完整同步性能。

## M2 聚焦验证

新增 records_http、records_store、photo_graph，以及 migrations 单元测试/执行中 SIGKILL 测试；更新受影响启动/持久化测试。M2 新增 21 个测试，总计 35。覆盖 A04 的条件并发与 A05 的 atomic 回滚，以及幂等/变化索引/元数据历史基础；其余完整同步验收仍按阶段推进，不能将纯图测试当成实际资源发布成功。详情见 [M2](m2-records.md)。
