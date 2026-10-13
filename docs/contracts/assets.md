# 上传会话、资源描述与下载协议 v1

状态：设计已冻结，HTTP/文件系统验证待 M4。原生扩展 namespace 由 Bearer 账户和配置确定，不能任意传 scope。

## 接口

| 方法与路径 | 认证 | 行为 |
|---|---|---|
| POST `{CloudKit-base}/assets/upload` | Bearer | 创建会话，返回 tokens[].url 与会话扩展 |
| POST `/v1/uploads/{sessionId}/content` | 上传能力 URL | 原始字节，流式校验和发布 |
| GET `/v1/uploads/{sessionId}` | Bearer | 查询持久会话状态 |
| POST `/v1/uploads/{sessionId}/renew` | Bearer | Reserved 会话重新取得上传 URL |
| GET `/v1/assets/{assetId}?role=original|thumbnail` | Bearer | 新的资源描述、下载 URL 和 receipt |
| GET/HEAD `/v1/assets/{assetId}/content?role=...` | 下载能力 URL 或 Bearer | 指定角色内容/元数据，不重算 hash |

本服务上传 URL 使用 application/octet-stream 原始字节，不实现 Apple 私有 multipart 加密上传；兼容 `/assets/upload` 的通用申请形状。不能宣称未适配 Apple SDK 可完成上传。

## 申请（固定）

```json
{"zoneID":{"zoneName":"PrimarySync"},"tokens":[{"recordName":"master-001","recordType":"CPLMaster","fieldName":"resJPEGThumbRes","_syncserver":{"role":"thumbnail","expectedSize":1024,"expectedSha256":"0000000000000000000000000000000000000000000000000000000000000000"}}]}
```

expectedSize 必须正整数且在角色限制内；expectedSha256 为可选小写 64 位十六进制。recordName 和 fieldName 必需，原图 fieldName=resOriginalRes，缩略图=resJPEGThumbRes；未知 role/字段组合拒绝。一个请求最多 200 tokens。

tokens[].url 是绝对上传 URL。`_syncserver` 响应固定含 sessionId、state=Reserved、expiresAt、urlExpiresAt；见[完整 fixtures](fixtures.json)。Idempotency-Key 将整个申请与规范指纹绑定，逐 token 派生稳定子键。

一个会话有效 24 小时；上传/下载能力 URL 有效 15 分钟。已接受接收的凭证到期不取消在途请求，但受空闲 30 秒/总时长 3600 秒默认限制。会话未过期且 Reserved 时 renew 接受空 JSON，返回同 sessionId 与新 url/urlExpiresAt，不改变期望字节。Published/Failed/Expired 不再 renew。

## 上传与状态

请求体按原始字节接收，Content-Length 若提供必须等于 expectedSize，实际输入必须精确匹配；超过限制立刻终止。同一会话只有一个 Receiving 所有者，竞争请求返回 409 UPLOAD_BUSY。

上传校验失败或连接中断进入 Failed；v0.1 没有上传续传，客户端创建新会话重新传输。发布意图已经持久化时按服务器恢复协议继续，不能因 URL 到期清理该意图。

上传成功 HTTP 200，形状固定为 `{"singleFile": <资源描述符>}`。状态 GET 返回 sessionId、state、asset（仅 Published）、serverErrorCode（无错误为 null）。Receiving/Verified/PublishPrepared/ObjectPublished 是处理中，客户端继续查询；只有 Published 可以绑定记录。

重复 POST 已 Published 会话返回 409 UPLOAD_ALREADY_COMPLETED，不接收新正文；客户端 GET 状态取得成功，不把不同正文误认为第一次上传。响应丢失不影响 Published receipt。Failed/Expired 使用对应错误，无隐式重置。

## 资源描述符（固定）

| 字段 | 格式 |
|---|---|
| assetId | 不透明 UUID，命名空间内稳定 |
| size | 正整数实际字节数 |
| fileChecksum | 实际 SHA-256 的标准 RFC 4648 Base64，32 字节、带 padding（44 字符） |
| checksumAlgorithm | 字符串 SHA256 |
| mimeType | 服务端验证类型；未知原图为 application/octet-stream，不信任输入 Content-Type |
| width/height | 有界解析可确认时返回正整数，否则省略；缩略图必需 |
| downloadURL | 当前角色的绝对有限期 URL |
| receipt | 服务端签名资产证据，绑定 namespace/generation/assetId/摘要/size |

此处 generation 指 database_generation，不是照片 contentGeneration。receipt 不绑定唯一照片或角色；资源复用时由数据库核对 Published 状态与 thumbnailValidated。缩略图声明的 sourceOriginalSHA256 属于照片绑定，不属于共享资产全局属性。

描述符只接受上表字段；wrappingKey/referenceChecksum 等 Apple 加密字段明确 BAD_REQUEST，不静默忽略。降低配置上限只影响新的上传，不使已按 v1 上限验证的资产失效。

SHA-256 Base64 与十六进制是同一摘要的不同编码；禁止把任意 ETag 或客户端声明当作内容证据。上传流计算摘要一次，并验证可选 expectedSha256。去重时元数据保留角色验证属性；未被验证为 thumbnail 的已发布对象不能直接充当缩略图。

服务端在上传 thumbnail 时验证 JPEG/PNG、长边不超过 2048、文件不超过 2 MiB；仅有界解析，不全文解码原图。相同 bytes 已存在但缺 thumbnail 验证时可通过新 thumbnail 上传补齐验证，仍只保留一个对象。

持久记录和 operation receipt 存规范化资产引用，不存会过期的 URL/证据。幂等响应重渲染 URL/receipt，保留原 revision，不产生变化。

## 下载条件

强 ETag 固定为双引号包裹的 `sha256:<小写十六进制>`，由元数据生成。If-Match 强匹配失败返回 412；If-None-Match 命中返回 304。条件判断先于 Range。

GET 支持一个 bytes 范围，包括 start-end、start-、-suffix；越界、零 suffix 或多段范围返回 416 及 Content-Range。成功部分响应为 206。HEAD 忽略 Range、返回完整长度，不读取正文。If-Range 强 ETag 匹配时返回范围，否则完整 200；不支持的日期形式按不匹配处理。

原图下载使用 attachment；未知类型为 application/octet-stream。thumbnail 使用服务端验证的 JPEG/PNG MIME。能力 URL 绑定 AssetId、role、GET/HEAD 与到期时间；修改 role/路径拒绝。过期后 Bearer GET 描述重新取 URL，不修改记录版本。

## 错误和资源上限

不存在/其他 namespace 的 asset/session 返回 HTTP 404 UNKNOWN_ITEM；未认证 401；上传大小超限 413；队列满 429/503；业务状态冲突 409。兼容入口使用 serverErrorCode/reason，原生入口同样返回该错误对象。

v0.1 默认原图上限 512 MiB、缩略图上限 2 MiB；原图上限可配置。JSON 上限 1 MiB；上传 tokens 最大 200，记录修改/lookup 按[记录契约](records.md)最大 400；zone 数组每请求最大 20、页大小 1–200。URL 到期时间使用 Unix 秒；JSON expiresAt/urlExpiresAt 用 UTC RFC 3339。

资产 receipt 与快照/token 的签名密钥跨重启稳定；密钥轮换或数据库恢复后旧证据可失效，账户通过元数据接口取新证据。URL 不进入日志。所有例子凭证均为合成占位，不能访问真实服务。

## 上行协调新增约束（未实现）

所有变更路径附带[上行租约](upload-coordination.md)凭证，上传能力/会话绑定 leaseId/fence。`tokens[]._syncserver` 增加 intent=create/update（对应新增/修改阶段）；预留/续期/正文及 Published 成功确认都在当前代次和阶段内，不能用仍有效的上传 URL 跨越租约失效。现有 fixture 描述合成结构，不能视为 M4 已通过。

会话/资源/receipt 状态 GET 和下载继续无须上行租约。上传完成不直接清本地 dirty，照片记录对的变更结果确认并经有效本地回调后才更新字段。阶段顺序固定删除→修改→新增；入库对象和服务器摘要规则不变。
