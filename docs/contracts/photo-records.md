# 照片记录、字段与引用生命周期 v1

状态：M4 已实现可信资产与原图/缩略图成对正向绑定，包含真实 HTTP/文件测试；M2 历史验证边界保留在对应小节。JSON 样例见[fixtures](fixtures.json)中的 photoPair 与 createPhotoRequest。

## 一对一身份

一个活跃 CPLAsset 对应且只对应同 namespace、同 zone 的一个活跃 CPLMaster；反向也唯一。photoId 就是 CPLAsset.recordName。CPLMaster.recordName 是独立稳定 ID。

这是一对一的首版取舍，不声称等于 Apple 全部 CPL 模型。不同照片可使用不同 Master 引用同样的不可变资产字节。

recordName 不可变；保留过墓碑的名称不能用于 create，返回 RECORD_NAME_RETIRED。需要重建时分配新照片/资源记录名称，可复用保留的 Published 资产。一般 Record 同样不复用退休名称。

## 精确字段

字段使用 `{type,value}`。

| 记录 | 字段 | 类型与规则 |
|---|---|---|
| CPLMaster | photoId | STRING，必须等于其唯一 CPLAsset.recordName；创建后不可改 |
| CPLMaster | contentGeneration | STRING，UUID；绑定当前资源组合 |
| CPLMaster | thumbnailSourceOriginalSHA256 | STRING，小写 64 位十六进制；等于 resOriginalRes 的服务端摘要 |
| CPLMaster | resOriginalRes | ASSETID，可信原图资源描述符 |
| CPLMaster | resJPEGThumbRes | ASSETID，可信缩略图描述符；字段名保留，内容允许 JPEG/PNG |
| CPLAsset | masterRef | REFERENCE，value 含 recordName、action=NONE；zoneID 可省略，但不能跨 zone |
| CPLAsset | contentGeneration | STRING，与 Master 相同 UUID |
| CPLAsset | relativePath | STRING，安全相对路径；同 zone 活跃照片间唯一 |
| CPLAsset | isFavorite/isHidden | 可选 INT64，只接受 0/1，默认 0 |
| CPLAsset | assetDate | 可选 TIMESTAMP，UTC Unix 毫秒整数 |

未知非保留字段可按通用 CloudKit 字段规则保存，不改变引用和权限；保留字段不能删除或改错类型。recordType 首版不可改；测试未来需要类型变化时另行扩展契约。

relativePath 不能为空、绝对路径、含空段、`.`/`..`、反斜线或 NUL；不能进入本服务/客户端保留工作目录。v1 保留首段 `.syncserver`、`.photoviewer-sync`、`.sync-staging`。保留原有 Unicode，不隐式重命名。重命名只改此字段，冲突返回 PATH_CONFLICT，不能覆盖另一照片。

## 资产身份与 generation

资产对象只存实际字节的 ID、摘要、大小及服务端验证属性，不拥有全局 contentGeneration 或唯一角色。generation 属于 photo_bindings/记录版本；相同对象可以跨照片/generation 复用，也可在符合约束时同时承担两角色。

原图必须 Published；缩略图必须 Published 且 thumbnailValidated=true，由有界 JPEG/PNG 解析验证尺寸/大小。角色和权限从数据库核对，不能信任传入 downloadURL 或 mimeType。

ASSETID 写入描述符含 AssetId 与服务端 receipt，并带 size、fileChecksum、checksumAlgorithm；与已发布元数据不一致时拒绝。持久化只保留资产身份与角色，签名 URL/receipt 不进入版本内容或请求指纹。详见[资源协议](assets.md)。

同一 generation 不允许更换任何角色 AssetId。更换组合必须提供新 generation，同时更新两条记录；只改收藏、标题、路径等元数据无需更新 Master 或 generation。服务端核对 thumbnailSourceOriginalSHA256，不重新转码原图证明视觉派生，客户端对生成关系负责。

## 创建与内容更新

- 创建照片：同 zone `atomic=true` 的两个 create；两个资源先 Published，事务最终状态必须满足完整一对一图。
- 内容更新：同 atomic 批次 update Master 与 Asset，两者都带当前 recordChangeTag；保持 photoId/masterRef，替换资源组合和 generation。
- 元数据更新：可单独条件 update Asset；图不变时允许非 atomic。
- 不通过“先建一半，后面补”产生对客户端可见的照片，也不允许通用 CRUD 绕过保留类型约束。

事务在应用全部操作后验证最终图，再提交 records、record_versions、changes、photo_bindings 和 receipt；输入操作顺序不影响最终合法性。删除批次和创建批次可任意排列两记录操作。

## 删除、解绑、重建

1. 删除照片要求同 atomic 批次 delete Asset 与 Master，两条当前 revision 都必需。去除当前绑定并为两记录产生墓碑，不删除资产字节。
2. 单独删除 Master、单独删除 Asset、单独改 masterRef、清空资源字段均返回 PHOTO_PAIR_REQUIRED；整项/atomic 批次无变化。
3. v0.1 不支持 unlink、跨 zone 移动或将已有 Asset 改绑另一个 Master；需要此行为使用删除旧对和创建新 ID 的完整批次，所有版本条件仍成立。
4. atomic=false 的成对创建/删除/内容更新不允许，逐项返回 PHOTO_PAIR_REQUIRED；不能降级为半成品。
5. 重建使用新 recordName，仍提供两个 Published 资产；已有 bytes 可复用，不再次存储。新关系具有新 generation。

photo_bindings 必须有唯一键 `(namespace,zone,photo_id)` 和 `(namespace,zone,master_record_name)`，并校验两侧活跃记录和资源。历史版本保留旧资源组合，不用当前绑定反向重写历史。

## 空间分类归属

[空间查询](storage-quota.md)将 CPLMaster 的原始资源及缩略图作为同一逻辑文件的占用，分类取原始资源的可信 MIME，未知时读取 photo_bindings 对应 CPLAsset 的 relativePath 扩展名。视频使用 JPEG/PNG 缩略图时，缩略图字节仍归入 videoBytes；图片的归入 imageBytes，未知文件的归入 otherBytes。同一缩略图被多个文件引用时按每份逻辑引用分别计量，用户无须感知独立缩略图分类。

## 验收

创建/删除顺序；孤儿/跨 zone；不同 revision 并发；非 atomic 拆对；重复路径；退休名称；metadata-only 不增加资源；相同资产跨 generation 去重；共享资产删除后其他照片仍可下载。

## M2 验证边界

纯图测试和真实 HTTP 拆对拒绝已验证；服务端不接纳客户端自称已发布的 ASSETID 描述。M2 尚无可信资源来源，完整照片创建返回 UNSUPPORTED_CAPABILITY 而非写入半成品；真实资源确认、持久引用归一化及正向配对发布验收留到 M4。

## 分类上行和冲突时间补充

[协调契约](upload-coordination.md)规定删除→修改→新增；配对 delete/update/create 分别位于对应阶段。metadata-only 不上传资源；同时含内容/元数据修改的同一照片合并为 atomic 对更新。重命名互换仍按最终图验证，不通过暂时释放路径破坏一对一。删除后重建使用新 ID，不能复用退休名称。

冲突基于既有基线及独立 metadata/content/deletion 事件时间；文件 mtime/assetDate 不能替代全部修改语义。未来删除墓碑补充 deletedAt，当前 M2 未保留该字段。dirty 是本地 DB 字段，阶段开始或单资源成功不清它。
