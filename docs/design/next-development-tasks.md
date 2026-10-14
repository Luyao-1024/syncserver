# 下一步开发任务：M3 上行协调与增量基础

日期：2026-10-14。状态：以下 M3/M4 服务端任务已进入实现并配备运行测试，M5 CI/部署工具已建立；交付状态见[实现说明](v0.1-server.md)。清单中的真实 PhotoViewer/C1 任务仍未实施。首次发布前不新增旧开发版数据库兼容任务。

## M3-A：先完成 token 与真实下行进度

| 任务 | 计划涉及边界 | 完成标准 |
|---|---|---|
| A1 | credentials/domain 的公共 token codec | 域分离 HMAC，scope/kind/generation/position/H/marker 语义校验；错类错误区分，普通重启不失效 |
| A2 | store/api 的 snapshot/query/lookup | CPLAsset/CPLMaster 共用 snapshotToken/H，历史 keyset 分页、墓碑后过滤，禁止 OFFSET 冒充快照 |
| A3 | database/zone 变化 | 固定 H 分页、完整不可变版本、显式墓碑、末页进度、无变化与跨 zone 水位语义 |
| A4 | 事件时间模型和 wire 扩展 | 保存 metadata/content/deletion 时间、来源/稳定决胜信息，delete 不能丢事件时间；兼容 fixture 明确扩展 |
| A5 | 合成客户端接收示例 | inbox/待办/ingest 同事务，完整投影和范围内协调后才推进 appliedToken；不是 C1 实际交付 |

聚焦验收：分页期间并发 create/update/delete，跨类型同 H，atomic 对跨页，空 zone/仅其他 zone 变化，篡改/错 scope/代际恢复和重复无变化查询。涉及资产正文的完整成本验收仍等 M4。

## M3-B：再完成租约与上行门槛

| 任务 | 计划涉及边界 | 完成标准 |
|---|---|---|
| B1 | schema/domain/store 租约 | namespace 唯一持有者，持久 fence，90 秒 TTL，正常重启失效旧租约且 fence 增长 |
| B2 | api 获取/续租/释放 | 一次获取失败跳过语义、续租不过期复活、独立控制配额、旧 release 不能释放新持有者 |
| B3 | check 最新进度 | 真实 zone token 语义+服务端当前 H+本地 localReady，落后保持 Checking；其他 zone 变化不空拉死循环 |
| B4 | phase/metadata mutation guard | Delete→Modify→Create 单向转换，所有 records/modify/别名统一门槛及最终 fence 检查，保留条件/图/幂等 |
| B5 | operation 状态读取 | 无锁读成功或 Pending 子结果；响应丢失/失锁恢复不重复创建、不提前清 DB dirty |
| B6 | 轮次中止的合成双设备模型 | 失锁后当前/所有未完成项报错，迟到成功不清 dirty；获取失败仅跳过上行，多端读仍成功 |

先将 API/领域/数据库的真实边界做完再启用能力；没有通过进度门槛的变更不允许因旧 M2 客户端兼容性而直接写入。不得把单 SQLite 写入线程当作设备租约。

## M3-C：接口整合与阶段交付

补齐已有 aliases/query batch；能力声明包括实际租约参数、token/gate/phase/状态读取的支持位。更新 M2 测试 fixture：租约有效时 CRUD/条件/回滚/幂等行为仍成立，缺锁/未追平/阶段不符的写入明确拒绝；测试通过不暗示 C1 已实现。

通过最终代码树 fmt/clippy/build/test 后提交推送。M5 才配置远端 CI/部署，不用本地测试宣称公网已经工作。

## M4 接续任务

- 会话、上传 URL 和成功确认绑定 leaseId/fence/intent；正文接收随失锁取消。
- 保活跨多个 TTL 能服务大文件；正文 idle/total 限额仍独立；首版网络中断仍按会话状态重传。
- Metadata 修改复用资源；CONTENT 才上传变化资源，并同 generation 绑定原图/缩略图。
- 发布持久意图各窗口故障/失锁测试；稳定对象不因中止删除，旧租约不增加成功绑定。
- 多端 HEAD/GET/Range 不取上行锁，空查询/HEAD 的已有正文读取与 hash 增量为 0。

## C1 客户端接续任务

仅在明确进入客户端接入阶段后修改 PhotoViewer：

1. 读其 AGENTS 与 sync/storage/viewer 模块文档，将 dirty/本地版本/基线关联放在既有同步 DB 记录里，删除条目确认前保留。
2. 配置范围内先下行，获取失败一次跳过；获取后保持独立保活并用 appliedToken 锁内补拉至 current。
3. 冻结 DB 派生视图，删除/修改/新增；元数据与内容合并，生成稳定资源快照；新本地修改留下一轮。
4. 成功确认回调按 job/config/cycle/operationId/本地版本更新字段，区分逐项错误/AlreadyCurrent，失锁后迟到回调无效。
5. 双资源发布/缓存/索引屏障、独立修改时间与冲突副本、内部事件不重置 dirty；本地范围退出/删除抑制继续有效。
6. 实际客户端 DB/传输/真实 UI 输入验证，不能用服务端模拟替代。

所有验收的统一来源是 [spac.md A24–A33](../../spac.md)、[上行协调契约](../contracts/upload-coordination.md)与[验证计划](verification.md)。

## 当前后续工作

服务端基本功能完成后，先看最终提交的完整闸门、exact-SHA CI 和部署工具证据。实际公网安装/运维、客户端 C1、多账户/分享、推送、上传续传和大规模基准按各自范围推进，不混同为已完成。
