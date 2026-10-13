# 资源生命周期

状态：待实现。目标是摘要只在入库/显式校验时计算，轮询不重读正文。

## 会话与描述符

`assets/upload` 为指定 namespace、recordName、fieldName 创建有界上传会话，返回有限期 uploadURL。实际字节通过原生资源入口流式传输，不把图片嵌入 JSON。

CloudKit 风格回包保留申请上传 URL 的结构；资源描述符在首版定义为本服务的明确子集，包含 size、SHA-256、AssetId 和签名 downloadURL。`fileChecksum` 可按适配约定编码为 SHA-256 的 Base64，同时显式带 `checksumAlgorithm=SHA256`；这不是对 Apple 私有 checksum 算法的断言。

Apple 的 wrappingKey、referenceChecksum、加密 receipt 等未核验私有机制不伪造。客户端只支持不透明资源描述的行为时可以解析；依赖特定 Apple 加密行为的请求明确不支持。

## 状态转换

```text
Reserved → Receiving → Verified → PublishPrepared → ObjectPublished → Published
                ↘ Failed / Expired
```

- Reserved：会话及约束已持久化。
- Receiving：随机暂存文件接收字节，同一遍计算 SHA-256 和大小。
- Verified：输入长度、限制和可选客户端期望摘要校验通过。
- PublishPrepared：可恢复的意图已持久化，暂存文件已 fsync。
- ObjectPublished：对象文件已稳定发布，等待确认数据库状态。
- Published：资产行及结果 receipt 已提交，可被记录引用。

只有 Published 可引用。响应成功不早于稳定文件和数据库提交。

## 文件存放

对象在数据目录内按 namespace 和 SHA-256 派生路径存放，路径由服务端生成。客户端文件名、recordName、zoneName、URL 都不直接拼接到文件系统路径。

暂存必须与对象位于同一文件系统，使用随机名字、排他创建和拒绝符号链接的打开方式。发布采用不覆盖已有对象的原子操作，fsync 文件与父目录；现有对象的大小/资产行必须一致，否则报告存储损坏而不是信任。

同 namespace 的同内容只保存一份对象；不同 namespace 不做可观察的跨账户去重。文件对象一旦发布不可原地改写。

## 原图与缩略图

照片发布要求 original/thumbnail 两个角色。首版上传客户端从同一稳定原图生成并上传缩略图，服务端验证缩略图的有界类型、尺寸和字节限制；不在查询中生成预览。

资源可先后上传，但绑定事务只有两角色 Published 后才成功。下载按 role 返回精确资源；缩略图缺失必须报错，不偷偷发送原图。缩略图请求保留独立有界资源额度。详见[完整媒体同步](media-sync.md)。

## 记录提交

上传成功返回绑定 namespace、AssetId、摘要和大小的服务端证据。记录 create/update 验证证据及 published 状态，在事务中写版本和变化。上传资源与保存记录是两个明确阶段。

上传成功但记录保存失败时，资源保留，客户端可重试记录提交。首版不做 GC，因此不会误删尚未被引用或被历史版本引用的资源。

## 下载

GET/HEAD 只读取可信资产元数据；GET 流式发送对象字节。强 ETag 由持久 digest/AssetId 生成，不通过每次下载或 HEAD 重算正文获得。

下载支持单段 Range、If-Range、If-Match/If-None-Match 的明确条件；非法或越界范围返回对应错误。下载 URL 绑定 namespace、AssetId、允许方法和到期时间。URL 过期时可重新取得描述符，不改变记录 revision。

Range 仅提供下载续传，不表示已经实现块差异同步。上传续传在 v0.1 能力声明为 false。

## 幂等与回收

同 operation_id 与相同请求指纹返回已提交结果；不同请求指纹返回冲突。响应丢失后客户端先查询会话结果，避免盲目重传。

只回收已确认过期、未发布且无进行中操作的会话暂存文件。资源 GC 和历史裁剪均不在首版范围内。数据库和资源一起备份，不能只备份 SQLite 文件。

故障窗口见[事务和恢复](concurrency-recovery.md)。
