# CloudKit 风格接口兼容契约

状态：M2 已实现 zone 列表、元数据 modify/当前 lookup 与基础接口；快照/变化/资产仍为计划子集。不能用于宣称完整 iCloud 或 CloudKit 兼容。上位规格：[spac.md](../../spac.md)。

## 范围与路径

基础路径为 `/database/1/{container}/{environment}/{database}`，首版仅开放配置的 container、`production`、`private`。测试可使用 `com.apple.photos.cloud` 作为 container 名称；不代表拥有 Apple 的身份和数据。

原生扩展使用 `/v1/`。所有数据请求使用本服务的 Bearer 认证；签名上传/下载 URL 使用有限期能力凭证。Apple ID 登录、SRP、Cookie 和原生 Photos 客户端登录不属于兼容范围。

实际 iCloud 请求可以使用 `Content-Type: text/plain` 携带 JSON。因此兼容 POST 接受 `application/json` 和 `text/plain`，仍执行 JSON 长度和结构检查。

## 矩阵

| 路径（基础路径之后） | 方法 | v0.1 计划 | 边界 |
|---|---|---|---|
| `/zones/list` | POST | 列出已配置 zone | zone 创建/删除仅部署初始化，不开放修改接口 |
| `/records/query` | POST | recordType、desiredKeys、一致分页 | 只支持明确列出的排序/过滤；Hyperion 索引不支持 |
| `/records/lookup` | POST | 按 recordName/zoneID 批量读取 | 显式返回不存在的项目 |
| `/records/modify` | POST | create、条件 update、条件 delete | 强制覆盖类型不支持；atomic 只支持单 zone |
| `/changes/database` | POST | 哪些 zone 有变化、database token | 不代表分享权限或多账户 |
| `/changes/zone` | POST | zones 数组、记录变化、墓碑、moreComing | 每 zone 独立 token |
| `/records/changes` | POST | `/changes/zone` 的旧接口别名 | 仅相同已核验请求形状，不兼容未知历史变体 |
| `/internal/records/query/batch` | POST | 基本查询批次 | 不实现 Apple 的 HyperionIndexCountLookup 等私有计数索引 |
| `/assets/upload` | POST | 分配上传 URL | Apple 加密/receipt 私有行为不支持，描述符见资源文档 |

原生扩展：`GET /healthz`、`GET /readyz`、`GET /v1/capabilities`、`POST /v1/snapshots` 与固定的上传/资产路径。完整路径、JSON 与边界以[共享快照](../contracts/snapshots.md)、[照片记录](../contracts/photo-records.md)、[资源协议](../contracts/assets.md)为冻结契约。

能力声明必须包含协议版本、摘要算法、可用操作、最大页数、历史保留模式与不支持项。只有实现且验证的能力才可标记为 enabled。

## 记录

记录形状保留 `recordName`、`recordType`、`zoneID`、`fields`、`recordChangeTag`。`fields` 保留 CloudKit 风格的类型和值，服务端不猜测 Apple 的加密字段含义。

```json
{
  "recordName": "photo-001",
  "recordType": "CPLAsset",
  "zoneID": {"zoneName": "PrimarySync", "ownerRecordName": "_defaultOwner"},
  "recordChangeTag": "opaque-revision",
  "fields": {"isFavorite": {"type": "INT64", "value": 1}}
}
```

recordType 创建后不可改；已退休 recordName 不复用，重建使用新 ID。通用业务错误中的 RECORD_NAME_RETIRED/PATH_CONFLICT/PHOTO_PAIR_REQUIRED 是本服务扩展。

同一 zone 的 recordName 稳定标识对象；文件名和路径是普通字段。`recordChangeTag` 为不透明 revision。它不是 hash，也不是可供客户端排序的数字。

创建不允许覆盖已有记录。更新/删除要求当前 revision；不匹配返回 `CONFLICT` 及可用的服务端当前记录。删除返回墓碑，不自动删除资产字节。

同一修改批次不允许对同一 recordName 提交多次操作。`atomic` 未指定时默认为 true。`atomic=true` 的单 zone 批次整体成功或回滚。`atomic=false` 的每个操作独立提交，响应按请求顺序返回成功或错误；后一操作可观察前一操作的已提交结果。跨 zone atomic 请求拒绝，不能悄悄降级。

## 照片角色扩展

照片使用 CPLMaster 资源描述与 CPLAsset/masterRef 的受支持子集，并绑定 original/thumbnail 与 content generation。两记录一对一关联；发布、内容更新及删除在单 zone atomic 批次的最终图中验证。元数据更新可单独执行，跨 zone/拆对/解绑拒绝。字段、生命周期与共享资产规则见照片记录契约，不能将通用 CRUD 当作绕过照片完整性的方法。

缩略图与原图 AssetId、digest、size 各自独立。预览 URL 不指向原图。具体字段扩展和下载流程见[完整媒体同步](media-sync.md)与[资源生命周期](asset-lifecycle.md)。

## 变化请求与响应

```json
{
  "zones": [{
    "zoneID": {"zoneName": "PrimarySync"},
    "syncToken": "opaque-zone-token",
    "resultsLimit": 200
  }]
}
```

```json
{
  "zones": [{
    "zoneID": {"zoneName": "PrimarySync", "ownerRecordName": "_defaultOwner"},
    "syncToken": "opaque-next-zone-token",
    "moreComing": false,
    "records": [{
      "recordName": "photo-deleted",
      "recordType": null,
      "recordChangeTag": "opaque-delete-revision",
      "deleted": true
    }]
  }]
}
```

空 token 从保留历史起点枚举；首版不裁剪历史。没有变化返回空 records、`moreComing=false` 和相同已追平 token。客户端不能仅因 records 为空判断追平。

`changes/database` 接受顶层 syncToken，返回顶层 `syncToken`、`moreComing`、`zones`。zone 项带 `zoneID`；database token 不能当作 zone token 传回。客户端仍用各自保存的 zone token 获取变化。

## 首次清单

`records/query` 支持 recordType 精确匹配、recordName 升序和 desiredKeys。首版不支持用户自定义 sortBy/filterBy，也不实现 `CPLAssetAndMasterByAssetDate`、startRank 等私有组合索引；相关请求明确拒绝。

跨类型初次同步先取得 snapshotToken，所有类型共用 H；query/lookup 接受该顶层扩展。未指定快照时第一次请求捕获自己的 H，不能假设多个独立查询水位相同。后续 `continuationMarker` 绑定 H 和查询参数。所有页都返回 H 对应的 zone syncToken；之后客户端从 H 获取变化。因此扫描期间的创建、更新和删除不会被混合页面覆盖或漏过。

只从当前投影做 OFFSET 分页不符合此契约。具体过程见[同步状态机](sync-state-machine.md)。

## 基本请求形状

以下示例仅展示通用记录，不绕过照片成对绑定约束。

### zone 列举

```json
{}
```

```json
{"zones": [{"zoneID": {"zoneName": "PrimarySync", "ownerRecordName": "_defaultOwner"}, "deleted": false}]}
```

### 查询

```json
{"zoneID": {"zoneName": "PrimarySync"}, "query": {"recordType": "Album"}, "resultsLimit": 200, "desiredKeys": ["title"]}
```

```json
{"records": [], "syncToken": "snapshot-zone-token"}
```

有后续页时增加 continuationMarker；追平页省略 marker。控制字段仍保留。

### 修改

```json
{
  "zoneID": {"zoneName": "PrimarySync"},
  "atomic": true,
  "operations": [{
    "operationType": "create",
    "record": {"recordName": "album-001", "recordType": "Album", "fields": {"title": {"value": "Travel", "type": "STRING"}}}
  }]
}
```

```json
{"records": [{"recordName": "album-001", "recordType": "Album", "zoneID": {"zoneName": "PrimarySync", "ownerRecordName": "_defaultOwner"}, "recordChangeTag": "opaque-revision", "fields": {"title": {"value": "Travel", "type": "STRING"}}}]}
```

update 只改请求指定字段；replace/forceReplace 不在首版支持范围。删除请求的 record 携带 recordName 与当前 recordChangeTag。普通 lookup 输入 records 数组，元素包含 recordName 与可选 zoneID。

### database 变化

```json
{"syncToken": "opaque-database-token"}
```

```json
{"syncToken": "opaque-next-database-token", "moreComing": false, "zones": [{"zoneID": {"zoneName": "PrimarySync", "ownerRecordName": "_defaultOwner"}, "deleted": false}]}
```

### 幂等扩展

HTTP `Idempotency-Key` 映射为域内 operation_id，与整个规范化请求指纹绑定。非 atomic 批次派生稳定的逐项 id，局部提交后重试不能重复创建变化；单项成功也保存 receipt。该 header 是本服务扩展，不要求未修改的 Apple 客户端提供。

## 参数与错误

`desiredKeys` 只裁剪字段，不删除 recordName、recordType、revision、zoneID 或 deleted 等控制字段。`resultsLimit` 必须为正整数，最大 200；批次和 zones 数量也有有界限制。

`getCurrentSyncToken=true` 被识别并按上述查询边界返回 token。`clientId`、`remapEnums` 只作为已记录的兼容提示，不改变权限、字段类型或加密语义。其他会改变查询结果的未知参数拒绝。

CloudKit 风格业务错误使用 `serverErrorCode` 和 `reason`；逐项操作错误保留 recordName。未认证、请求过大、限流和服务不可用仍使用 HTTP 401/413/429/503。`AUTHENTICATION_REQUIRED`、`BAD_REQUEST`、`UNKNOWN_ITEM`、`CONFLICT`、`EXPIRED_SYNC_TOKEN` 等映射必须有 fixture，不能用 `EXPIRED_SYNC_TOKEN` 隐藏数据库故障。

## 兼容证据

已观测接口形状来自 [kei 调研资料](open-source-reference.md)。记录修改和资源上传参考 Apple 官方 Web Services 文档。首版只承诺本矩阵内的请求/响应，不能承诺未经适配的 kei、icloudpd 或 Apple Photos 完整工作。

## M1 实际支持状态

只开放 `GET /healthz`、`GET /readyz`、Bearer 认证的 `GET /v1/capabilities`，以及基础路径下的 `POST /zones/list`。zone 列表接受空 JSON 对象，Content-Type 为 application/json 或 text/plain（可带 charset）。未知选项、数组和畸形 JSON 返回 BAD_REQUEST，不忽略参数。

错误格式为 serverErrorCode/reason；未认证返回 401 AUTHENTICATION_REQUIRED，其他 container 返回 404 UNKNOWN_ITEM，development/public/shared 及尚未实现的已知接口返回 501 UNSUPPORTED_CAPABILITY。未知接口返回 404，zone 列表错误方法返回 405。所有响应附带 no-store、x-request-id 和 nosniff。

M1 能力声明逐项列出 false 的快照/记录/变化/资源能力；不生成 cursor、receipt 或资源签名 URL。协议和资源 JSON fixture 的文档检查仍不构成其接口已实现的证据。

## M2 实际支持状态

新增 records/modify 与 records/lookup，统一接受 JSON/text/plain 并拒绝重复 JSON 键。支持字段、补丁语义、条件与错误、nonatomic 幂等恢复和固定限额以[记录契约](../contracts/records.md)为准。能力声明 recordCrud=true，并返回支持字段类型、协议版本/算法/保留模式和记录限额。

snapshots/incrementalChanges/assetUpload/assetDownload/photoPublication=false。records/query、带 snapshotToken 的 lookup 和资源正向发布未开放；M2 不将当前读取冒充跨请求一致清单。M1 小节是上一阶段支持范围的历史记录，不应作为当前 capability 列表。

## 新增上行协调扩展（设计冻结，当前不开放）

[协调契约](../contracts/upload-coordination.md)定义 upload-leases 获取/续租/进度/阶段/释放和只读 operation 状态。它们属于本服务原生扩展，不声称是 Apple 私有协议。M3 实现 token/租约/门槛后统一守卫 records/modify 及所有后续变更入口；M4 会话与上传能力绑定同一 fence。下行 query/lookup/changes/资源读取不持上行锁。

启用时声明 uploadLease/fencing/uploadProgressGate/uploadPhases/operationStatusRead 等能力与实际 TTL/保活参数；只有实现且测试通过才置 true。M2 当前无这些能力、请求头或状态读取入口，本轮仅文档冻结。新阶段只接受同类 operationType 批次，不能把原 M2 混合批次支持保留成跨阶段旁路。
