# 同步与游标状态机

状态：待实现。关联[协议](protocol-compatibility.md)与[数据模型](data-model.md)。

## 五种游标/能力

1. database sync token：说明 namespace 内已检查到哪个位置，用来发现变过的 zone。
2. zone sync token：说明某 zone 已获取到哪个变化位置。
3. query continuation marker：继续同一固定快照和查询，不可用于变化查询。
4. snapshotToken：不同 recordType 的查询/lookup 共用固定水位，不用于推进增量。
5. 上传/下载能力凭证：授权资源操作，不表示同步进度。

签名负载包含 version、kind、database_generation、namespace、zone（若适用）、position、水位和查询指纹。使用服务器持久密钥与域分离签名；对 MAC 做恒定时间验证。客户端不能解析或自行构造受支持的 token。

## 首次同步

```text
客户端发现 zone
  → POST /v1/snapshots 一次捕获水位 H
  → CPLAsset/CPLMaster 的 query/lookup 共享 snapshotToken
  → keyset 分页读取 H 时刻的记录状态
  → 每页返回相同 H 的 zone syncToken
  → 客户端保存清单和待办后提交 H
  → changes/zone 从 H 获取扫描期间及之后的变化
```

采用[共享快照契约](../contracts/snapshots.md)：创建快照的事务捕获 H，不同类型各用独立 marker，但共享 snapshotToken/H。marker 绑定 desiredKeys、recordType、排序、namespace、zone；不得把一类查询完成视为整个清单完成。没有 snapshotToken 的独立查询仍自行捕获 H，但不能直接用于跨类型完整图库 bootstrap。

如果客户端中断首次扫描，它需保存已收到的页与 marker，再继续；不能把“收到第一页的 H”当作“全量清单已完整”。尚未完成的清单不得产生删除推断。

空 zone 也返回有效 H 的 token。即使 H=0，后续首次写入仍可被捕获。

## zone 增量

已追平 token 表示位置 P，下一次请求捕获水位 H。每页只读取 `P < seq <= H` 范围内的本 zone 变化，返回不可变版本，包括显式墓碑。moreComing 为 true 时，下一 token 保留 H 并推进 P。

atomic 批次可跨变化页；逐页持久化 inbox 和 ingest token，但在本轮 moreComing=false 前不把暂定投影交给照片发布/删除协调。最终验证同 H 的完整图再生成资源待办。详见共享快照契约。

最后一页推进到 H，转为已追平 token。没有新变化时返回原 token，records 为空。因过滤或未来历史压缩出现空页时也不能仅据 records 长度判断结束；应以 moreComing 为准。

v0.1 不做按 desiredRecordTypes 的增量过滤，避免略过客户端后来需要的类型；该提示不被当作可靠过滤保证。desiredKeys 仅裁剪业务字段。

## database 增量

没有 token 时列出所有已配置 zone，返回当前 namespace 水位。以后按 namespace 的变化序号索引分批读取并去重本页 zone，返回本页覆盖位置的下一 token。

同一个 zone 可在多页出现，客户端需合并；token 推进到已扫描的变化位置，不能推进到尚未覆盖的位置。database 已追平并不证明每个 zone 的记录已获取，zone token 独立管理。

## 客户端可靠接收（接入契约）

```text
请求变化 → 验证响应 → 同一客户端事务保存清单、待办、token
                           ↓
                     异步处理上传/下载
                           ↓
                     更新对应内容基线
```

下载失败不回退已可靠保存的 ingest token，但必须保留待办。缩略图与原图分别有持久待办；只有两个资源校验并稳定发布后才入库和建立成功基线。中间状态仅存在于同步清单，详见[完整媒体同步](media-sync.md)。服务端样例测试需演示这一接收顺序；PhotoViewer 具体数据库实现由后续接入任务承担。

## 失效与恢复

签名错、种类错或 namespace 错为请求错误。数据库 generation 不同、位置超出当前水位或未来低于保留边界，返回明确的 EXPIRED_SYNC_TOKEN 并要求重建元数据清单。

常规进程重启不改变 generation/密钥，因此不使有效 token 失效。备份恢复需显式更新 generation，避免旧 token 跳过恢复后重新使用的序号。

重建清单不等于重传所有资产：客户端可复用同 AssetId/digest 的稳定内容。不能仅因 token 失效推断任意记录删除。
