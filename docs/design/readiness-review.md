# 开发准备度与设计闭环

日期：2026-10-13。状态：M0 设计阻塞项已补齐；M1/M2 已实现并有本地 HTTP/事务/进程测试，详见 [M1 基础](m1-foundation.md)与 [M2 记录](m2-records.md)。A01–A23 的完整同步实现验收仍待后续阶段。

| 问题 | 固定决策 | 设计状态 | 实现阶段 |
|---|---|---|---|
| 跨记录类型首次清单 | 独立 snapshotToken，共用 H；各类型独立 marker；收齐后提交清单；增量半批次只进入 inbox | 已闭环 | M3 |
| 照片关联与删除 | 一对一字段/路径唯一性；同 atomic 最终图验证；成对删除；退休 ID 不复用；资产 generation 属于绑定 | 已闭环 | M2 |
| 资源接口 | 固定 URL/JSON、SHA-256 Base64、会话状态/期限、renew/状态查询与凭证刷新 | 已闭环 | M4 |
| 客户端入库屏障 | 持久 sync_publications、统一索引准入、本地读取 lease、LocalReady 事务提交点与恢复 | 已闭环 | C1 |

## 可重复验证的材料

- [共享快照](../contracts/snapshots.md)
- [照片记录](../contracts/photo-records.md)
- [资源协议](../contracts/assets.md)
- [合成 JSON 与 PNG fixtures](../contracts/fixtures.json)
- [客户端发布](client-publication.md)
- [ADR 0003](../adr/0003-freeze-bootstrap-photo-and-asset-contracts.md)
- 文档检查命令：`python tools/check-docs.py`，仅检查链接、JSON 与 fixture 一致性，不能替代服务测试。

## 开发中需要验证的事情

SQL 查询计划、真实并发、哈希/读取计数、故障恢复、尺寸解析、token 验证、客户端 watcher/lease 与真实 UI 仍需按[验收计划](verification.md)实现并执行。这些是已经有明确约束的实施工作，不是待讨论的接口选择。

多账户、Apple 私有索引/登录、上传续传、历史裁剪、资源 GC、跨实例部署维持暂缓；不在开发过程中悄悄扩围。

## M2 后续推进

M1 已建立 Rust crate、锁定依赖、固定配置/认证/scope/健康/就绪/能力声明及数据库执行器和迁移基础。M2 已完成元数据记录与事务，照片图规则已校验；下一阶段按[实施计划](implementation-plan.md)推进 M3 的共享快照和变化分页，资源发布仍待 M4。当前阶段不替代其余阶段或 C1 的验证。
