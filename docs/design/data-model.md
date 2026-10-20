# 数据模型

状态：当前 schema v5 已建立记录/历史/变化、事件、租约、资源、会话与发布意图。发布前不承诺开发版数据库迁移兼容。[规格](../../spac.md)与[事务约束](concurrency-recovery.md)共同约束实现。

## 身份与版本

| 类型 | 语义 |
|---|---|
| NamespaceId | tenant、container、environment、database 的稳定范围身份 |
| ZoneId | 命名空间内逻辑集合；zoneName 是兼容名称 |
| RecordId | zone 内 recordName；路径不参与主键 |
| Revision | 记录状态版本，外部使用不透明 recordChangeTag |
| Digest | 资源实际字节的 SHA-256，附算法与 server-verified 来源 |
| ContentGeneration | 同一照片原图/缩略图组合的内容版本，独立于收藏等元数据 revision |
| CommitSeq | 已提交变化的有序位置；首版由 SQLite 事务分配 |
| SyncToken | namespace/zone、种类、位置和水位的签名封装 |
| QueryMarker | 查询参数指纹、固定水位和最后一个主键的签名封装 |
| AssetId | 不可变资源身份，摘要去重仅在命名空间内进行 |

不能把 checksum、revision、sync token 或 query marker 互相替代。

## 逻辑表

| 表 | 主要字段与作用 |
|---|---|
| server_meta | schema_version、database_generation，重启保持稳定 |
| namespaces | tenant/container/environment/database，唯一约束与配置绑定 |
| zones | namespace、zone_id、zone_name、owner；配置初始化 |
| records | 当前活跃投影：zone、record_name、record_type、revision、最新 seq、字段 |
| record_versions | 不可变版本：namespace、zone、record_name、seq、revision、完整持久字段、deleted |
| namespace_counters | namespace、last_committed_seq；事务内分配，复合 seq 身份为 namespace+seq |
| changes | namespace+seq、zone、record_name、version 引用；与版本同事务写入 |
| assets | namespace、asset_id、sha256、size、MIME、尺寸、thumbnailValidated、内部对象位置、published 状态；没有全局照片 generation |
| photo_bindings | namespace、photo_id、CPLAsset/CPLMaster、content generation、original/thumbnail AssetId；与照片记录同事务 |
| upload_sessions | 会话、namespace、期望限制、已接收状态、暂存位置、到期时间 |
| publish_intents | 持久发布意图、暂存/对象身份、摘要、状态、失败原因 |
| operation_receipts | namespace、幂等 operation_id、请求指纹和已提交结果 |

唯一性：同 namespace 内 digest/size 的已发布资产唯一；同 zone 内 recordName 唯一且退休后不复用；活跃 relativePath 唯一；photo_bindings 的 photo_id/master_record_name 各自唯一；operation_id 在 namespace 内唯一；同版本的变化不能重复提交。

records 是查询投影，不是历史的唯一来源。删除移除/标记当前投影，同时保留含 revision 的墓碑版本与变化。

## 提交顺序

变化序号在数据库事务中分配。revision、版本、变化、当前投影和成功 receipt 在同一事务提交。不能先分配可见序号再在另一个事务写入内容。

一个 atomic 批次按操作顺序分配序号，共同提交；回滚不留下可见版本。不同请求的可见顺序以提交序号为准，不以客户端时间排序。

## 一致分页所需的历史

在水位 H 读取每个 recordName 最后一个 `seq <= H` 的版本，过滤墓碑后得到清单。必须先解析该水位下的最后版本，再按 recordType 过滤；不能先筛类型再错误地找到更旧的版本。

首版保留所有版本、变化和已发布资产。跨请求快照无需长时间保持 SQLite 事务，也不会因并发更新使旧版本资源消失。在线裁剪属于后续独立设计，必须先定义 min-retained-seq、token 失效和快照/下载租约。

## 索引与查询计划

- changes：`(namespace, seq)`、`(namespace, zone, seq)`。
- record_versions：`(namespace, zone, record_name, seq DESC)`。
- records：范围/名称及 recordType 的查询索引。
- assets：`(namespace, sha256, size)`、`(namespace, asset_id)`。
- 会话与发布意图：状态/到期索引。

增量以 seq 范围索引读取，不连接扫描全部记录或全文 JSON。初次快照使用 keyset 分页，不用 OFFSET。真实 SQL 和 EXPLAIN 需要在 M2 用有历史版本的数据验证，不能仅凭列出索引宣称复杂度已满足。

## 照片完整性

照片记录按[冻结字段与图约束](../contracts/photo-records.md)实现一对一，generation 属于绑定/版本；不能将共享资产行限定成唯一照片或 generation。原图与缩略图必须同 namespace、同绑定 generation 且 Published；缩略图另需 thumbnailValidated。一般记录不强制照片角色。content generation 改变时两角色及 CPLAsset/CPLMaster 绑定在单 zone atomic 事务中更新。

收藏/标题等元数据更新不生成新 content generation，也不重新上传资源。历史版本保留其对应角色引用，不能渲染成另一版本的缩略图。详见[完整媒体同步](media-sync.md)。

## 资产字段

持久字段只保存可信 AssetId、摘要和大小，不保存会到期的签名下载 URL。API 返回时渲染下载 URL；URL 刷新不产生记录变化、不更新 revision。

所有结构化资产引用必须校验 namespace、published 状态和大小/摘要。客户端发来的任意 URL 只可作为普通字符串，不能被服务端当成可读取资产或触发外部下载。

SQLite 的路径不是外部 API 标识，禁止向客户端泄露真实数据目录。

## M1 物理 schema

M1 的 schema v1 只包含 server_meta、namespaces、zones、namespace_counters 和 service_starts。SQLite application_id 标识本应用，user_version 和 server_meta.schema_version 一致；拒绝已有未知应用或新版 schema。初始化以单事务创建命名空间、zone 和 0 水位，普通重启不重建表或更新 generation。后续阶段通过新迁移补充记录/资产表，不提前以空表宣称这些语义已实现。

## M2 物理 schema

schema v2 增加 records、record_versions、changes、photo_bindings 和 operation_receipts。当前记录以 deleted 墓碑保留退休名称；历史以 (namespace,seq) 唯一，当前 seq 通过延迟外键绑定对应历史。namespace_counters 在同事务推进；启动检查历史/变化最大 seq 与计数器一致。

operation_receipts 主键为 (namespace,operation_id,item_index)；-1 为整个请求，nonatomic 子项使用非负索引。result_json 为空表示 Pending，只绑定指纹；每个子结果与相应记录事务一起提交，之后完整根结果稳定保存。

在 M2 历史阶段，资产表与正向确认未开放；M4 当前已验证与归一化资产描述，临时 URL/receipt 不进入记录/历史/指纹。索引已用实际 HTTP 数据和 EXPLAIN 验证，证据见 [M2](m2-records.md)，不等于 M3 整体快照分页已经验收。

## 上行协调与客户端字段（服务端已实现，客户端待 C1）

当前 `upload_lease` 单行同时维护 namespace 的当前持有者、leaseId/deviceId、固定 zones、服务器单调期限、显示期限、phase/verifiedHead 和持久 fence；不以 metadata generation 充当租约代次。`records/record_versions.events_json` 保存独立 metadataEvent/contentEvent/deletionEvent（editedAtMs/source/eventId）。`assets`、`upload_sessions`、`publish_intents`、`upload_reservations` 保存已发布摘要/尺寸/缩略图资格、lease/fence/intent、持久发布/隔离意图和资源预留幂等；实现见[基本服务端](v0.1-server.md)。

C1 的 dirty 是本地既有同步记录上的字段，另有本地变更版本、组别时间、远端身份/基线和必要的 operationId 关联。没有独立 dirty 文件/表/持久队列；下载待办、远端 inbox 与 sync_publications 继续负责不同职责，不是 dirty 镜像。

租约阶段是 Delete → Modify → Create；新增时 NEW 可保留后续 META/CONTENT 位，成功创建确认登记身份后新义务转成修改/删除。callback 在同一 DB 事务登记确认并只清已完成部分，必须核对轮次与本地版本。删除条目在确认前仍有数据库记录，不能因为原图已从磁盘删除就丢掉 dirty/身份/时间。

同步进度字段分 ingestToken/appliedToken；上行门槛检查后者，不要求本地 dirty 为 CLEAN。详细不变量见[上行协调](../contracts/upload-coordination.md)。


## M5 运行期指标（2026-10-15）

[运维指标](../contracts/metrics.md)存于进程计数器，不增加持久表、不分配媒体 seq、不改变 schema v5。运行标识复用本次 service_starts 的 run_id，与同步 generation 和租约 fence 独立。每次服务实例启动重新计数；队列长度和传输占用为采样值，不构成提交或客户端进度证据。


## 回收站投影（schema v7）

schema v7 为 `photo_bindings` 增加由 `CPLAsset.isTrashed` 派生的 `trashed` 列：与其它展示字段在同一记录事务写入，升级时由写路径同一 `index_row` 规则回填。控制台普通照片列表、相册、时间线与汇总只读 `trashed=0`，回收站相册通过 `trashed=true` 读取同一投影。该列只是读取索引，不进入记录版本、变化流、同步 token 或空间配额；回收站照片仍是活跃记录，永久成对删除才移除绑定。


## 多用户扩展的数据归属

各用户的上述照片 schema 独立存放；不把多个账户用一个可任意传入 tenant 的请求混在同一接口上下文。普通用户 tenant 为不可变用户 UUID；唯一显式接管者保留旧 namespace 与代际。认证库另有 schema 1：users 保存 UUID、唯一用户名、Argon2id PHC、授权版本、enabled 和 legacy 所有权；sessions 保存随机秘密的 SHA-256、用户 UUID、授权版本、绝对过期时间。不保存明文密码或会话。详见[认证契约](../contracts/authentication.md)。


## 用户资源空间计量

[空间配额](../contracts/storage-quota.md)复用 schema v5 的当前 `records.fields_json/deleted`、可信 `assets.size`、`upload_sessions` 与 `upload_lease`，不新增持久计数器或修改 generation。每用户全部 zone 共享上限；当前未删除记录的每个 ASSETID 字段按资产大小分别计量，不同文件引用相同 AssetId 也累加，历史/墓碑/未引用对象不计入。有效未完成上传仅预留相对当前字段的正增长。对象发布不直接增加已用；记录事务以最终逻辑占用检查增长，超额回滚投影、版本、变化及成功 receipt。删除立即释放逻辑配额，物理对象仍保留。


空间分类查询把当前 ASSETID 引用与同账户资产元数据、同 zone 的 photo_bindings 关联，原始资源 MIME 优先，无法分类时按当前虚拟路径扩展名划分图片/视频/其他。缩略图及关联资源跟随文件原始资源的分类；同一缩略图的多个引用各计入对应文件类别。三个分类大小之和为逻辑已用，不新增 schema、持久计数器或正文扫描。具体字段与扩展名规则见[配额查询契约](../contracts/storage-quota.md)。

配额查询的实现不得让 SQLite 对每个 JSON 字段重新扫描资产表：存储线程先读取当前记录字段并解析 ASSETID 引用，再按 `(namespace, asset_id)` 主键批量读取资产元数据。该路径只访问元数据，不读取或重新哈希对象正文。
