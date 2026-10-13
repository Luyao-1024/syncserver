# M2 记录事务与持久化

日期：2026-10-13。M2 已实现；完整同步仍待 M3/M4，发布与部署待 M5。

## 实现结构

- `domain/records.rs`：独立 RecordName/Revision/CommitSeq 类型，严格输入、类型字段、补丁合并和版本条件。
- `domain/photos.rs`：受影响照片组件的最终一对一图校验、稳定身份、generation、角色组合和路径规则；只用元数据。
- `store/migration_v2.sql`：当前记录、历史、变化、照片绑定和幂等表，以及范围索引/外键/唯一键。
- `store/records.rs`：当前 lookup、atomic/nonatomic 事务、版本序号分配与幂等恢复，均在原有有界数据库线程运行。
- `api/records.rs`、`api/json.rs`：CloudKit 风格适配、幂等头、严格 JSON（含重复键拒绝），复用 M1 认证/限额/范围。

接口、字段子集与限额见[元数据记录契约](../contracts/records.md)。兼容能力以[矩阵](protocol-compatibility.md)和运行时 capabilities 为准。

## 数据库与事务

从 schema v1 在单事务升级到 v2，保留 namespace、zones、generation 和签名身份；范围/密钥不一致先拒绝，不先迁移。启动核对计数器、历史和变化最大序号，不对对象目录做扫描或哈希。

条件写入先按主键加载记录/受影响伙伴，构造候选最终图和响应预算，再分配 seq/revision 并同时写当前投影、历史、变化、绑定和 receipt。删除保留当前墓碑及历史，阻止复用 ID。当前投影到对应历史版本的外键延迟至事务提交校验，防止只写投影而缺失版本。不存在全图库扫描、正文读取或网络调用。

nonatomic 为每项独立事务；Pending 根 receipt 绑定完整请求，子 receipt 与每项记录事务共同提交。数据库故障或杀进程之后可用原键恢复未完成项，已完成项不重放。已接受的命令遵守 M1 关闭排空规则，不因 HTTP 客户端取消而撤销正在提交的事务。

## 照片与资源

图校验覆盖配对创建/删除、内容更新、metadata-only、主从稳定身份、同 zone、混合 generation、路径唯一性和声明摘要关联。photo_bindings 按 scope 对 photo/master/path 各有唯一约束；组件通过名字索引加载。

M2 尚无可信 Published 资产来源，所有 ASSETID 写入在入库前返回 UNSUPPORTED_CAPABILITY。纯图测试使用冻结合成描述符验证关联逻辑；不制造 Published 数据库状态，也不作为原图/缩略图传输已通过的证据。M4 会接入实际资源验证、持久引用归一化和描述符渲染。M2 不修改 PhotoViewer。

## 验证证据

本次新增 21 个测试，另更新受影响的 M1 测试。已执行的聚焦测试通过：

- 实际 HTTP CRUD、补丁删除、版本冲突/并发、退休名称、atomic 回滚、nonatomic 逐项结果。
- 幂等并发/规范化/重启复用，以及完整请求指纹冲突。
- 纯照片图的创建顺序、缺角色/跨 zone/混合 generation、metadata-only、成对删除与重复路径，以及稳定资源引用不依赖临时 URL/receipt。
- SQL 故障注入：投影/历史/变化/计数器/成功 receipt 同步回滚；nonatomic 中途故障重启只继续未完成项。
- 真正子进程 SIGKILL：重试 400 项 nonatomic 批次，最终恰好 400 记录/历史/变化，首项 revision 不变。
- 历史 schema v1 升级及拒绝范围时不部分迁移。
- 通过 HTTP 建立 400 条记录、800 历史版本，EXPLAIN 验证 namespace/zone 变化及单记录历史范围查询命中索引。
- 重复 JSON 键/请求头、未知能力/参数、400 项上限及累积字段大小超限。

最终代码树推送闸门均 PASS：

- `cargo fmt --all --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo build --locked --all-targets`
- `cargo test --locked --all-targets`（35 个测试）
- `python tools/check-docs.py`（35 Markdown、152 本地链接、18 JSON 示例、2 PNG/hash fixture、7 畸形配对检查）
- `git diff --check`

远端 CI、正式部署、增量游标/一致清单、实际资产上传发布和 PhotoViewer UI 验收仍未进行。
