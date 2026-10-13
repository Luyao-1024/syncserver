# M2 元数据记录接口 v1

状态：M2 已实现；日期：2026-10-13。基础路径 `/database/1/{container}/production/private`；Bearer 认证。接受 application/json 或 text/plain JSON，统一正文限额和 30 秒接收上限。拒绝重复 JSON 对象键、未知参数和错误类型；元数据端点不支持 URL query 参数。

## 修改请求

`POST .../records/modify`。zoneID 默认 PrimarySync/_defaultOwner；record 内可指定配置中的 zoneID。atomic 默认 true，仅允许一个 zone；atomic=false 可跨已配置 zone。同 zone 同 recordName 在一个批次中不能出现两次。

```json
{
  "zoneID": {"zoneName": "PrimarySync"},
  "atomic": true,
  "operations": [{
    "operationType": "create",
    "record": {
      "recordName": "album-001",
      "recordType": "AlbumMetadata",
      "fields": {"title": {"type": "STRING", "value": "旅行"}}
    }
  }]
}
```

operationType 支持 create/update/delete，forceUpdate/forceDelete 等返回 UNSUPPORTED_CAPABILITY。create 必须有 recordType，不接受 recordChangeTag 或 null 字段。已有活跃记录返回 CONFLICT；已退休名称返回 RECORD_NAME_RETIRED。

update/delete 必须有当前 recordChangeTag；检查与写入处于同一事务。recordType 不可改。update 的 fields 是补丁：未提供的字段保留，null 删除普通字段；合并后的完整字段再次校验。delete 不接受非空 fields，成功返回有新 revision 的墓碑。每个接受的 update 均生成新 revision/变化，包括字段值相同的条件更新。

```json
{
  "operations": [{
    "operationType": "update",
    "record": {
      "recordName": "album-001",
      "recordChangeTag": "current-opaque-revision",
      "fields": {
        "title": {"type": "STRING", "value": "新的标题"},
        "obsolete": null
      }
    }
  }]
}
```

正常结果为 HTTP 200 `{"records":[...]}`，每项含 recordName/recordType/zoneID/recordChangeTag/fields。删除项含 deleted=true、recordType=null，不含 fields。

atomic 任一错误整体回滚，以业务 HTTP 状态返回 serverErrorCode/reason，CONFLICT 附 serverRecord（可为最新墓碑）。atomic=false 按序独立事务，HTTP 200 records 数组含逐项成功/错误，错误项带 recordName/zoneID。数据库或 I/O 故障返回 500；已提交的前项保持有效，有幂等键时可恢复重试。

## 读取请求

`POST .../records/lookup` 读取当前已提交状态，同一次请求使用一致的读取事务；不是跨请求快照。支持请求/逐记录 zoneID 和 desiredKeys。缺失或已删除记录逐项返回 UNKNOWN_ITEM。

```json
{
  "records": [{"recordName": "album-001"}],
  "desiredKeys": ["title"]
}
```

desiredKeys 缺省返回全部字段，空数组返回空 fields，不隐藏身份/revision。snapshotToken 和分页参数在 M2 明确拒绝，M3 才开放。records/query 仍返回 501，不能把当前 lookup 当作完整初次清单。

## 通用字段子集

支持 STRING、INT64、DOUBLE、TIMESTAMP、BYTES、REFERENCE 及其 `_LIST` 形式。INT64/TIMESTAMP 为有符号 64 位整数，DOUBLE 必须可表示为有限 f64，BYTES 为标准带 padding 的规范 Base64。LIST 最多 4096 项；每记录最多 256 个字段。

REFERENCE 使用 recordName/action=NONE/可选 zoneID，仅接受同 zone/_defaultOwner。普通引用不要求目标当前存在，也不触发级联删除；照片引用另执行完整图校验。省略或显式填写相同 zone/owner 在规范化指纹中等价。LOCATION、ASSETID_LIST 及其他字段能力不启用。STRING 中的 URL 只保存普通文本，不触发外部请求。

## 幂等与限额

Idempotency-Key 为单个请求头，1–128 个非空白可打印 ASCII 字符。命名空间内唯一；绑定规范化后的完整批次、atomic、顺序、实际 zone、版本条件和字段。不接受重复 header。键相同但请求不同返回 409 IDEMPOTENCY_CONFLICT。

atomic 成功结果与记录/历史/变化在同事务保存；失败整体回滚，不登记成功 receipt。nonatomic 先持久登记完整请求指纹，再逐项将结果与事务共同保存，最后保存完整响应。Pending 批次重试只执行缺少 receipt 的项，保留已提交的成功和业务错误结果；数据库故障项没有结果，不被伪装成完成。

已完成键返回原始域结果，即使后来记录又被修改/删除；无键的响应丢失需 lookup 核对。M2 不发行资源凭证；M4 接入前必须增加 ASSETID 验证及临时 URL/receipt 的归一化。

修改/lookup 最多 400 项。标识最长 255 UTF-8 字节且不含控制字符；合并字段 JSON 最多 1 MiB，结果上限 8 MiB。批次使用保守响应/工作集预算，在写入前拒绝超限；lookup 累计超限返回 413，不返回截断的成功结果。HTTP 正文仍受实际配置 jsonBodyLimit 约束。

## 照片边界

CPLAsset/CPLMaster 通过[照片图契约](photo-records.md)验证最终图，拆对、混合 generation、解绑和不安全路径拒绝。M2 所有结构化 ASSETID 写入在可信资源边界返回 501；没有 Published 资源来源，因此不会成功发布照片或把客户端描述符写入历史。正向双资源绑定/发布以及下载描述符渲染在 M4 实现。
