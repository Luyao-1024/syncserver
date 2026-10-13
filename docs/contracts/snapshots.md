# 共享快照与首次同步契约 v1

状态：设计已冻结，代码/HTTP 验证待 M3。它是本服务扩展，不是对 Apple 私有接口的额外承诺。

## 创建快照

`POST /v1/snapshots`，Bearer 认证；namespace 由账户及配置固定，不接受请求任意指定。请求只含 zoneID，缺省为 PrimarySync；ownerRecordName 缺省为 _defaultOwner，其他 owner 拒绝。

```json
{"zoneID":{"zoneName":"PrimarySync"}}
```

```json
{"zoneID":{"zoneName":"PrimarySync","ownerRecordName":"_defaultOwner"},"snapshotToken":"opaque-snapshot-token","syncToken":"opaque-zone-token"}
```

在同一读事务中捕获该 namespace 已提交水位 H，并产生 snapshotToken 与 H 的 zone syncToken。H 必须在已提交事务的边界，不能来自尚未提交的批次。

snapshotToken 绑定 namespace、zone、database_generation、H 和协议版本；kind=snapshot。v0.1 保留历史，快照不设置时间到期。它不绑定 recordType 或 desiredKeys，因此可供多个类型查询共用。它与 syncToken 不可互换。

## 使用快照

CloudKit 风格 `records/query` 与 `records/lookup` 增加顶层 snapshotToken 扩展；未提供时保留捕获当前水位/当前读取的行为。

```json
{"zoneID":{"zoneName":"PrimarySync"},"snapshotToken":"opaque-snapshot-token","query":{"recordType":"CPLAsset"},"resultsLimit":200}
```

查询 CPLMaster 时使用同一个 snapshotToken，但独立的 continuationMarker。所有读取从 H 时刻的历史版本得到结果，不能从当前 photo_bindings 投影替换历史资源关系。

marker 绑定 H、namespace、zone、recordType、desiredKeys、排序和最后 recordName；只用于这一查询。页大小不改变结果集，可在 1–200 内调整。snapshotToken 与 marker 同时提供时必须一致；否则 BAD_REQUEST。

同一 H 的不同查询返回相同 zone syncToken。lookup 也读取 H；H 时已删除或不存在的项目返回 UNKNOWN_ITEM。响应中的短期 downloadURL/receipt 可刷新，不改变 H、revision 或持久字段。

## 客户端流程（固定）

1. 发现 zone，创建一次快照 S，持久化 S 及“bootstrap 未完成”。
2. 使用 S 读完 CPLMaster、CPLAsset 及首版实际需要的其他类型，各自保存 marker；不将某一类型结束视为清单完整。
3. 收齐全部类型后验证一对一引用和资源对；同事务提交完整远端清单、双资源待办、S 的 zone token，并标记 bootstrap 完成。
4. 从该 zone token 获取后续变化。单项文件发布仍需双资源 LocalReady 门槛。
5. 不以不同类型最后一次请求返回的不同水位代替共同水位；不解析不透明 token 比大小。

关键例子：S 的 H=100，之后两记录在一次事务更新到 101/102；用 S 的两次查询都读旧完整对，随后从 H=100 获取新完整对。没有混合代际或跳过变更。

## 增量页面与 atomic 批次

变化页允许按记录数量切分一个 atomic 批次，但页面和 token 不代表可发布的完整照片视图。客户端逐页同事务保存 inbox、暂定投影和 ingest token；直到本轮固定 H 的 moreComing=false，再验证引用图并提升为可协调投影，生成该 H 的最终双资源待办。

已有图库继续显示旧完整版本。中断后从持久 ingest token 继续原轮 H；不得丢弃 inbox，也不得依据半批次进行下载替换/删除。每个 event 对应不可变记录版本，并按响应顺序应用；同记录多次变化最终收敛到 H 的最后版本。

## 配置与错误

zone 集合及 namespace 配置在数据目录首次初始化后固定；v0.1 不支持通过改配置在线新增/删除 zone。启动发现不一致返回 CONFIG_SCOPE_MISMATCH，不静默新增或丢弃数据。避免空 zone 的变化不经过记录流而被遗漏。

签名/种类/范围错为 BAD_REQUEST；generation 错或水位超过恢复后数据范围为 EXPIRED_SYNC_TOKEN；SQL 故障是服务错误。快照与 query marker 的有效性规则一致。

## 必须验证

跨类型查询间的成对更新/删除；分页中断；snapshot/marker 错种类与跨范围；快照 lookup；atomic 变化跨页并中断；配置 zone 变化；快照重启可用。

关联：[状态机](../design/sync-state-machine.md)、[照片记录](photo-records.md)、[fixtures](fixtures.json)。
