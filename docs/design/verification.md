# 验证计划

状态：M3/M4 基本服务端已有 token/分页/租约/文件/HTTP/恢复/合成接收测试；真实 PhotoViewer/C1 和公网部署另行验证。M1 执行证据见 [M1 基础](m1-foundation.md)。对应[spac.md 的 A01–A23](../../spac.md)。

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

## 上行协调新增验收（待 M3/M4/C1）

对应 spac.md A24–A33：

1. 两设备并发获取仅一个有效租约，失败方一次跳过，dirty 不变；多端下行仍成功。
2. 首次下行与取锁之间远端发生修改，锁内检查/重新下行追平；分页/跨类型/未完成资源不误判 ready。
3. 仅无关 zone 变化或 token 表示不同，无变化空页仍可语义追平；篡改/错 kind/错 scope/恢复 generation 错误明确区分。
4. 使用可控时钟跨多个 TTL 模拟大文件、独立保活和控制队列；不依赖长真实 sleep，不把保活当上传续传。
5. 失锁取消当前正文及所有未完成项并逐项报错；旧 fence 最终提交、迟到 release、迟到 HTTP 200 不写入/清字段。
6. 先前已确认成功不回滚；已提交响应丢失通过无锁状态读取恢复，不重复创建或清新 dirty。
7. Delete/Modify/Create 实际 HTTP 阶段约束；元数据操作无资源 I/O/hash，路径互换修改 atomic。
8. C1 用真实本地 DB 验证 dirty 字段+callback 版本竞争、NEW 期间新编辑/删除、nonatomic 部分成功、只读 AlreadyCurrent 确认。
9. 下行冲突副本、独立组别合并、deletedAt 仲裁、同步内部写入不产生 dirty；上传范围/本地删除抑制不被扩大。
10. 正常重启 fence 增长/旧租约失效，但同步 token/generation 正常保留；租约/续租不进入媒体 changes 或读取正文。

本次只修改设计/JSON fixture，不执行 Rust 或 HTTP 测试；新增行为不能借用之前 M2 的 35 个测试宣称已通过。

## 当前基本服务端验证

最终代码树执行完整 fmt/clippy/build/test；当前 58 个测试通过、无失败或忽略。新增 sync_http/leases_http/assets_http 与资产恢复单元测试验证固定水位、事件时间、别名/batch、字节预算、抢锁/持锁追平/单调过期、重启 fence、原图/缩略图真实上传与配对、100 次空变化/HEAD 零正文成本、Range/条件、失锁长流中止、跨多个 TTL 保活、持久接收事务失败/重启/双资源待办。既有 SIGKILL/nonatomic 恢复在新租约 fixture 下继续运行。

容器构建、smoke 和 exact-SHA CI 的结果以交付记录为准，不将模板或未执行命令记作通过。旧 M1/M2 小节保留为阶段历史。


## 2026-10-15：M5 运维可观测性本地验证

新增 5 个 metrics_http 测试，真实 HTTP 验证 Bearer/重复认证头、连续采样不增加数据库命令或正文成本、普通/控制 HTTP 准入饱和、三个变化别名的成功页/逐 zone 错误、重启计数重置与临时双 zone 服务上的 Python 部署 smoke。扩展 startup_http 验证执行器关闭后的认证诊断；扩展数据库队列测试验证控制队列容量/满拒绝/排空；资源测试通过新接口验证真实上传/下载、100 次空变化/HEAD 和活动上传配额释放。

本次最终 Rust 代码树的本地结果：

| 命令 | 结果 |
|---|---|
| `cargo fmt --all --check` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| `cargo build --locked --all-targets` | PASS |
| `cargo test --locked --all-targets` | PASS，63 个测试，0 失败/忽略 |
| `python tools/check-docs.py` | PASS，链接/JSON/合成 fixture 检查 |
| `git diff --check` | PASS |

只读 smoke 已由 metrics_http 对临时本地真实服务执行；这些结果不表示 systemd/容器或公网部署已验证。本次修改尚未提交/推送，新代码的远端 CI 和独立 MSRV 运行均 NOT RUN。旧 58 测试小节保留为先前阶段记录。


## 2026-10-15：阿里云真实部署验证

[部署记录](aliyun-deployment.md)列出实际运行的构建、systemd/Nginx、公网 smoke、HTTP 分块双资源往返、租约/照片对、零正文成本、重启 token、证书续期模拟与只读数据库完整性检查。服务当前有 0 条活跃记录、0 个暂存项，合成测试照片已删除，两份共 140 字节的资产按首版无 GC 规则保留。本次部署修改配置模板与文档，没有修改 Rust；新代码远端 CI 与 C1 仍未运行。
