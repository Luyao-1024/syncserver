# 每用户云端存储配额

状态：按逻辑大小计量的本地实现及测试已通过，证据见[验证记录](../design/verification.md)；线上部署另行验证。

本服务扩展；配额按认证账户的 Store 统计，覆盖该账户全部 zone。旧 API key 模式对应原单账户 Store。请求不能指定用户或 namespace。

## 配置与计量

`SYNC_USER_STORAGE_QUOTA_BYTES` 为每用户的总资源配额，默认 `20000000000` 字节（20 GB，十进制），须为 1 到 SQLite 有符号 64 位上限之间的整数，重启服务生效。所有用户独立享有相同配置上限；不是所有用户合计共享 20 GB。单文件原图及缩略图限制继续分别遵守 `SYNC_MAX_ASSET_BYTES` 与 `SYNC_THUMBNAIL_MAX_BYTES`。

- `usedBytes`：当前未删除文件记录所引用资源的逻辑大小之和。每个 ASSETID 字段引用按服务端资产大小计一次；不同记录/字段引用同一对象分别计量。原图和缩略图继续计入，同一文件幂等重试/重命名不重复计量，历史版本、未引用对象和已删除文件不计入。
- `reservedBytes`：有效租约代次下未完成、未过期的 Reserved/Receiving/PublishPrepared 上传的逻辑增长预留。新建按 expectedSize，更新按 `max(expectedSize - 当前同 zone/recordName/fieldName 的资源大小, 0)`；大小来自服务端资产元数据，不相信客户端摘要减免。
- `availableBytes = max(quotaBytes - usedBytes - reservedBytes, 0)`。
- 数据库、日志、历史对象、暂存及隔离恢复对象不属于用户逻辑占用。物理对象仍按内容去重并保留，逻辑配额与磁盘占用分开。

删除文件立即释放其逻辑配额，不自动删除仍被其他文件/历史引用的对象。降低上限不删除既有文件、不阻止读取/下载/删除/重命名、同大小替换或缩小；超额时禁止逻辑占用继续增长。相同内容的两份独立文件分别计量，跨设备同步同一记录只计一次。

## 上传准入与事务

创建上传会话时，在同一 SQLite 事务中检查 `usedBytes + reservedBytes + 本批逻辑增长预留 <= quotaBytes` 并写入全部会话。超额返回 HTTP 409 `STORAGE_QUOTA_EXCEEDED`，整批不创建会话、不保存成功幂等结果。幂等重试返回已有会话，不重复占用；失败、过期或失锁释放有效预留。允许不增加逻辑大小的更新，即使既有占用超过降低后的上限。实际接收字节仍必须精确等于 expectedSize，并由服务端验证 SHA-256。

对象发布只产生可被引用的内容资源，不直接增加用户 usedBytes；会话 Published 后结束传输预留。随后创建/复制/更新文件记录时，在记录/版本/变化/receipt 的同一事务中核对最终逻辑占用；占用增长必须满足 `最终 usedBytes + reservedBytes <= quotaBytes`。直接复用已发布资源也执行相同检查。超额 atomic 整批回滚，nonatomic 只回滚失败子项并返回逐项错误。删除/净缩小/等大小变更不因已超额被拒绝；成功结果的幂等重放不再次记账。

发布与文件记录之间允许暂时存在未引用对象；它不计入 usedBytes，也不保证后续创建文件一定成功，最终以记录提交事务为准。计算只读取元数据，不扫描目录、不读取或重新 hash 资产正文；空间查询不推进同步 generation/commit seq。

## 查询

`GET /v1/storage`，Bearer 认证，无参数，无须上行租约。拒绝任何 query 参数；只返回当前账户的配额。响应 `Cache-Control: no-store`，所有字节字段为非负整数；新增分类字段是已用逻辑空间的拆分，不包含 reservedBytes：

```json
{"quotaBytes":20000000000,"usedBytes":1000,"reservedBytes":2000,"availableBytes":19999997000,"imageBytes":400,"videoBytes":600,"otherBytes":0}
```

`imageBytes` 为图片占用，`videoBytes` 为视频占用，`otherBytes` 为其他或无法识别的占用；始终满足 `imageBytes + videoBytes + otherBytes = usedBytes`。用户不感知独立缩略图占用，接口不提供 thumbnailBytes；同一文件的原始资源及缩略图、其他关联资源都计入该文件所属类别。共享缩略图按每份逻辑引用分别计入对应文件类别，不按物理对象只计一次。

分类只读当前记录及可信资产元数据：原始资源 `resOriginalRes` 存在时，以它的服务端 MIME 为准，`image/*` 为图片、`video/*` 为视频；不能以 JPEG/PNG 缩略图的 MIME 将视频归入图片。原始 MIME 无法分类时，读取同 namespace/zone 的 photo_bindings.relative_path（或普通记录的 STRING relativePath）扩展名，忽略大小写。无原始资源字段的普通资源使用自身服务端 MIME 及记录路径。

扩展名分类为本服务展示规则，不是上传内容合法性的保证，不信任请求 Content-Type 或客户端描述符来覆盖服务端 MIME。支持图片扩展名 jpg/jpeg/jpe/png/apng/gif/webp/bmp/dib/tif/tiff/heic/heif/avif/jxl/ico/svg/dng/cr2/cr3/nef/nrw/arw/srf/sr2/orf/rw2/raf/pef/srw；视频扩展名 mp4/m4v/mov/mkv/webm/avi/mpeg/mpg/m2v/m2ts/mts/ts/3gp/3g2/ogv/wmv/asf/flv/vob；未匹配归入 otherBytes。对 MIME 未知的文件重命名时，类别随当前扩展名变化，总逻辑占用不变；可信原始 MIME 优先于路径扩展名。历史及未引用对象仍不计量。

查询在同一个数据库读事务中计算，沿用有界普通数据库队列。`GET /v1/capabilities` 增加 `capabilities.storageQuota=true`，`limits.userStorageQuotaBytes` 返回当前上限；新增 `capabilities.storageUsageByMediaType=true` 表示支持分类占用。使用量不放入零 SQL 的运维指标采样。

## 验收

默认与配置边界；精确达到上限及超额；批量/并发申请无超售；幂等不重复计量；失败/过期/失锁和重启释放预留；相同内容的独立文件重复计量；复用资源不能绕过记录事务配额；删除释放逻辑空间；同大小更新/缩小与降低上限保留下载；不同用户相同内容独立计量；无认证及指定其他用户被拒绝；查询不改变同步 token、不读取正文；图片/视频/未知资源混合计量，缩略图归属和共享引用、重命名/删除、重启、账户隔离及分类之和等于 usedBytes。

关联：[资源协议](assets.md)、[认证](authentication.md)、[兼容矩阵](../design/protocol-compatibility.md)、[恢复](../design/concurrency-recovery.md)。

## 物理安全预算（2026-10-23）

逻辑 usedBytes 的原有含义保持不变。新增 `SYNC_USER_PHYSICAL_STORAGE_QUOTA_BYTES`，每账户默认 40000000000 字节，独立限制全部已发布对象（含未引用/历史对象）、仍在接收/准备发布的资源、有效上传预留及真实存在的隔离对象。申请上传时在同一元数据事务计入完整 expectedSize，拒绝增长返回 HTTP 507 `PHYSICAL_STORAGE_QUOTA_EXCEEDED`；真实磁盘不足以保留 `SYNC_MIN_FREE_DISK_BYTES`（默认 1 GiB）时返回 507 `INSUFFICIENT_STORAGE`。正文开始接收前再次检查磁盘余量。

按可信对象元数据计量并按摘要去重，不读取/重新 hash 正文；仅对异常隔离对象执行身份路径的 stat。Receiving/PublishPrepared 不因租约失效立即放弃容量保护。成功发布后以实际资产元数据持续计量；失败/重启沿既有清理恢复协议释放暂存，普通重启不能绕过对象预算。已有对象不自动删除，无新增 GC。

每用户 metadata.sqlite 默认 1 GiB 物理预算（`SYNC_METADATA_MAX_BYTES`）；认证目录默认 64 MiB。达到 SQLite 上限时拒绝新增长，不裁剪记录、版本或墓碑。已有数据库大于新预算时明确拒绝配置，不能冒充缩容成功。WAL 定期 checkpoint；最终合计物理硬边界由部署的独立数据卷保障，避免仅靠文件大小和瞬时空闲量处理所有并发。
