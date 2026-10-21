# sync-client

`sync-client` 是 syncserver 云同步协议的 Rust 客户端 SDK。它只覆盖客户端能力：认证、能力探测、快照与增量读取、条件写入、上传租约、资源上传/下载，以及照片对的创建、修改与删除。

SDK **不包含**本地数据库、同步调度器、断点队列或持久化。同步 cycle 如何组织、哪些 token 落盘、失败后何时重试，由接入方决定。这一点与 PhotoViewer 的分层一致：SDK 负责协议，应用负责状态。

- crate：`sync-client`（lib 名 `sync_client`）
- Rust edition 2024，MSRV 1.88
- 主要依赖：tokio、reqwest（rustls）、serde/serde_json、sha2、base64、url、uuid
- 当前版本未发布到 crates.io（`publish = false`），通过 path 或 git 依赖接入

## 1. 接入

```toml
[dependencies]
sync-client = { path = "../syncserver/sdk" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

如果多个项目共享同一个仓库，也可以把 SDK 作为 workspace 成员或使用 git 依赖。

### 最小连接

```rust
use sync_client::Client;

async fn connect() -> Result<(), sync_client::Error> {
    let client = Client::builder("https://sync.example.com")?.build()?;
    client.ready().await?; // GET /readyz，未认证
    let session = client.login("alice", "secret").await?;
    let connection = client.connect().await?; // 探测并锁定服务端能力
    let zone = connection.primary_zone()?;
    println!(
        "signed in as {}; primary zone is {}",
        session.user.username, zone.zone_name
    );
    Ok(())
}
```

`Client` 持有 endpoint、HTTP 连接池和内存中的 bearer token；`Connection` 是探测成功后的会话，绑定并缓存 `namespace`、`zones`、`databaseGeneration`、能力位与限制。后续所有请求只使用已锁定的 scope，不会因为服务端某次返回而悄悄降级。

已经保存过 scope 的应用可以跳过探测：

```rust
use sync_client::Client;

async fn reuse(scope: sync_client::PinnedScope) -> Result<(), sync_client::Error> {
    let client = Client::builder("https://sync.example.com")?
        .access_token("token")
        .build()?;
    let connection = client.connect_with_scope(scope).await?;
    let zone = connection.primary_zone()?;
    println!("primary zone is {}", zone.zone_name);
    Ok(())
}
```

`connect_with_scope` 会校验 `publicBaseURL` 与配置 endpoint 同源，否则拒绝。SDK 不接受跨源资源地址，也不跟随 HTTP 重定向，避免 bearer token 和租约凭据被带到未配置的主机。

### 认证方式

- 账号模式：`login` 获取会话；所有 namespace 请求、资源上传/下载都同时携带 bearer。
- 旧 API key 模式：通过 `ClientBuilder::access_token` 配置；SDK 仍按 bearer 发送。
- `logout` 撤销当前会话并清除内存 token；服务端已拒绝的 token 也会被清除。
- SDK 不持久化 token。是否把 token 落到本地安全存储由应用决定。

## 2. 分层与模块

| 模块 | 内容 |
| --- | --- |
| `client` | `Client`、`ClientBuilder`、`Connection`、`Session`、能力探测 |
| `endpoint` | endpoint 校验（HTTPS、无 userinfo/query/fragment/path） |
| `scope` | `Capabilities`、`PinnedScope`、`Namespace`、能力位与限制 |
| `metadata` | snapshot、分页 query、lookup、database/zone changes、条件写入、operation receipt |
| `model` | `RemoteRecord`、`Field`、`AssetDescriptor`、`RecordOperation`、`ModifyRequest` |
| `lease` | `UploadLease`、`LeaseProof`、阶段、心跳与保守期限 |
| `assets` | 预留、上传、下载、会话状态、存储用量 |
| `photo` | 照片图校验、创建/内容替换/元数据更新/删除、确定性 operation id |
| `error` | `Error` 与服务端错误码映射 |

顶层 re-export 了常用类型，可以直接 `use sync_client::Client;`。

## 3. 元数据

### 3.1 快照与 bootstrap

`bootstrap` 读取一个 zone 的完整照片图。它先取固定水位的 `snapshotToken`，分别读完 `CPLMaster` 与 `CPLAsset` 的全部页，要求两半结束于同一个 `syncToken`，最后通过照片图校验：

```rust
use sync_client::Connection;

async fn bootstrap(connection: &Connection) -> Result<(), sync_client::Error> {
    let zone = connection.primary_zone()?;
    let graph = connection.bootstrap(&zone).await?;

    println!("{} photos at {}", graph.photos.len(), graph.sync_token);
    for photo in &graph.photos {
        println!("{} {}", photo.photo_id, photo.relative_path);
    }
    Ok(())
}
```

`syncToken` 是下一次增量读取的起点，应用必须持久化。`snapshotToken` 只在一次 bootstrap 内有效。

只读一种记录或需要自定义 `desiredKeys` 时：

```rust
use sync_client::{Connection, MASTER_RECORD_TYPE};

async fn page_masters(connection: &Connection) -> Result<(), sync_client::Error> {
    let zone = connection.primary_zone()?;
    let snapshot = connection.snapshot(&zone).await?;
    let masters = connection
        .query_all_records(&zone, MASTER_RECORD_TYPE, &snapshot.snapshot_token)
        .await?;
    println!("{} masters at {}", masters.records.len(), masters.sync_token);
    Ok(())
}
```

`query_page` 暴露单页与 `continuationMarker`；`query_all_records` / `query_all_with_keys` 自动翻页并检测重复 marker。所有分页都在同一个 snapshot 水位内进行。

### 3.2 增量 changes

```rust
use sync_client::{Connection, ZoneChangesRequest};

async fn changes(connection: &Connection, mut token: String) -> Result<(), sync_client::Error> {
    let zone = connection.primary_zone()?;
    loop {
        let page = connection
            .changes_zone(&ZoneChangesRequest::new(zone.clone()).token(token))
            .await?;
        for record in &page.records {
            // 应用有效记录；tombstone 的 deleted 为 true
        }
        for failure in &page.failures {
            // 逐项失败：不能把这一页当作完整
        }
        token = page.sync_token.clone();
        if !page.more_coming {
            break;
        }
    }
    Ok(())
}
```

- `changes_database` 返回发生变化的 zone；`deleted: true` 表示 zone 被删除。
- `changes_zones` 一次读取多个 zone，每个 zone 单独返回 `ZoneChangeOutcome`，允许一个 zone 失败而其他 zone 成功。
- `syncToken`、`continuationMarker`、`snapshotToken` 都是不透明字符串，SDK 不解析也不比较，只能原样回传。
- 查询只读元数据；照片正文不会参与轮询。

### 3.3 lookup 与单张照片

```rust
use sync_client::{Connection, Error};

async fn lookup(connection: &Connection, photo_id: &str) -> Result<(), Error> {
    let zone = connection.primary_zone()?;
    let photo = connection.fetch_photo(&zone, photo_id).await?;
    println!("{} -> {}", photo.photo_id, photo.relative_path);
    Ok(())
}
```

`fetch_photo` 按 asset → master 读取，并对这一对应用完整照片图规则；半对或代际不一致会直接报错，不会返回“看起来能用”的照片。`lookup_current` / `lookup_snapshot` 返回原始 `RecordResult` 列表，适合自行合并增量。

### 3.4 条件写入

低层写入以记录的 `recordChangeTag` 为条件。下面的 `zone`、`connection`、`lease` 来自应用上下文，`lease` 必须是当前 zone、当前阶段持有的租约（见第 4 节）：

```rust
use sync_client::{Field, ModifyRequest, RecordOperation, RecordResult};

let operation = RecordOperation::update(
    "record-name",
    "record-change-tag",
    [("isFavorite".to_string(), Field::int64(1))],
);
let request = ModifyRequest::new(zone, vec![operation])
    .idempotency_key("my-stable-operation-id");
let results = connection.modify(lease.proof(), request).await?;

for result in results {
    match result {
        RecordResult::Confirmed(record) => { /* record.record_change_tag 是新版本 */ }
        RecordResult::Failed(failure) => { /* 逐项拒绝 */ }
    }
}
```

`ModifyRequest::new` 默认 `atomic = true`，同一请求中的所有记录一起提交。HTTP 200 仍可能在响应体内携带 `serverErrorCode` 或逐项失败，SDK 统一映射为 `Error::InBand` / `RecordResult::Failed`。

`operation_status(operation_id)` 读取该幂等键对应的 receipt，不依赖租约，可在网络异常后用于判断“到底提交了没有”。

## 4. 上传租约

一个 namespace 同一时间只有一个有效的上行持有者。租约是跨设备互斥与 fence 的来源，SDK 把它封装为 `UploadLease`。

```rust
use sync_client::{Connection, Error, UploadLease, UploadPhase};

async fn open_lease(
    connection: &Connection,
    zone: &sync_client::ZoneId,
    applied_token: &str,
) -> Result<Option<UploadLease>, Error> {
    let lease = UploadLease::acquire(connection, "stable-device-id", zone).await?;
    let check = lease.check(applied_token, true).await?;
    if !check.is_current || !check.may_upload {
        // 本地还没有追上云端：先应用 changes，交出本轮租约。
        lease.release().await?;
        return Ok(None);
    }

    // check 通过后阶段从 delete 开始；每一步最多前进一格。
    while lease.phase() != Some(UploadPhase::Create) {
        lease.advance_phase().await?;
    }
    Ok(Some(lease))
}
```

要点：

- `acquire` 失败且错误为 `LeaseBusy`（`UPLOAD_LOCK_BUSY`）时，本轮应跳过，不要重试抢占。
- `check` 提交 `zoneTokens` 与 `localReady`。`isCurrent = false` 表示落后；`mayUpload = false` 时租约停留在 `checking`，追上后再 check。
- 阶段顺序固定为 `delete → modify → create`，SDK 与服务端都拒绝跳阶和回退。`advance_phase` 每次只前进一格，最后一格返回 `None`。
- `acquire` 会自动启动心跳任务；续租时间从请求发出时刻按单调时钟计算，并预留 10% 安全余量。迟到的续租不会复活已经失效的租约。
- `release()` 会停止心跳并释放租约；直接 `drop` 只停止心跳，不会发网络请求，所以正常结束请显式 `release`。
- 发生 `UPLOAD_LEASE_LOST` 后调用 `mark_lost()`，本轮不得再写；下一轮重新 `acquire`。
- `from_grant` + `start_heartbeat` 允许应用自己管理获取流程，但必须提供获得 grant 时的时间点。

阶段与操作对应：

| 阶段 | 允许 |
| --- | --- |
| `delete` | `delete_photo` |
| `modify` | `update_photo_metadata`、`update_photo_content`、以 update 意图预留资源 |
| `create` | `create_photo`、以 create 意图预留资源 |

因为 create 是最后一格，**同一租约内不能先创建再修改**。一个完整 cycle 通常先处理删除、再处理修改、最后处理新增；刚创建的照片在下一个 cycle 的 modify 阶段更新。

## 5. 资源

### 5.1 预留与上传

照片的两个角色（`original`、`thumbnail`）必须在同一个请求中预留，避免只创建一半：

```rust
use sync_client::{
    AssetRole, AssetUpload, Connection, ReservationRequest, UploadIntent, UploadLease,
};

async fn reserve(
    connection: &Connection,
    lease: &UploadLease,
    master_id: &str,
    original: &AssetUpload,
    thumbnail: &AssetUpload,
) -> Result<(), sync_client::Error> {
    let reservations = connection
        .reserve_assets(
            lease,
            master_id,
            UploadIntent::Create,
            &[
                ReservationRequest::new(AssetRole::Original, original.digest.clone()),
                ReservationRequest::new(AssetRole::Thumbnail, thumbnail.digest.clone()),
            ],
        )
        .await?;

    // 原图先传，缩略图后传。
    let original_descriptor = connection
        .upload_asset_file(
            lease,
            &reservations[0],
            &original.digest,
            "/photos/IMG_0001.HEIC",
        )
        .await?;
    let thumbnail_descriptor = connection
        .upload_asset_file(lease, &reservations[1], &thumbnail.digest, "/photos/thumb.jpg")
        .await?;
    println!(
        "uploaded {} ({}) and {} ({})",
        original_descriptor.asset_id,
        original_descriptor.size,
        thumbnail_descriptor.asset_id,
        thumbnail_descriptor.size
    );
    Ok(())
}
```

- 预留意图必须等于租约当前阶段：`Create` ↔ `create`，`Update` ↔ `modify`。
- `expectedSize` 与 `expectedSha256` 来自本地对实际字节计算出的 `ContentDigest`；服务端会重新读取上传字节并校验 SHA-256。
- `upload_asset` 接受内存字节，`upload_asset_file` 流式读取文件。两者都会在发送前检查本地 size/digest，并在响应后核对服务端返回的 descriptor。
- **没有断点续传。** 会话失败即终态，恢复方式是重新预留，而不是重试旧 session。
- `renew_upload_session` 只对仍处于 `Reserved` 的会话延长有效期并返回新的 capability。
- 若预留请求因幂等重放返回 `Published`，SDK 直接复用该 descriptor，不重复上传。

### 5.2 下载

```rust
use sync_client::{AssetRole, Connection, DownloadOptions};

async fn download(connection: &Connection, asset_id: &str) -> Result<(), sync_client::Error> {
    let fresh = connection
        .describe_asset(asset_id, AssetRole::Original)
        .await?;
    let bytes = connection
        .download_asset(&fresh, &DownloadOptions::new().range("bytes=1024-"))
        .await?;
    if bytes.continues() {
        // 206：追加到已有部分文件
    } else if bytes.not_modified() {
        // 304：本地已校验的副本仍然有效
    } else {
        // 200：完整响应；对 range 请求返回 200 说明资源已变化，不能追加
    }
    Ok(())
}
```

`describe_asset` 每次返回新的 `downloadURL` 与 `receipt`。`download_asset` 支持 `Range`、`If-Range`、`If-Match`、`If-None-Match`：

- `200`：完整响应；如果请求了 range 却收到 200，说明资源已变化，不能追加。
- `206`：范围响应。
- `304`：`If-None-Match` 命中。
- `412` / `416`：以带 `PRECONDITION_FAILED` / `RANGE_NOT_SATISFIABLE` 的 `Error::Api` 返回。

`download_to_file` 流式写入一个**不存在的**新文件，边写边计算 SHA-256，完成后校验 size 与 digest；失败时删除部分文件。它不会覆盖已有文件。

`storage_usage()` 返回账号配额与已用、预留、可用字节，以及按媒体类型拆分。

### 5.3 凭据边界

`downloadURL`、`receipt`、上传 capability URL 都是短期传输凭据：

- 只从 `describe_asset` / 预留响应 / 上传响应中即时取得；
- 不写入本地数据库，不作为资源身份；
- 资源身份是 `assetId + size + fileChecksum`（Base64 SHA-256）；
- SDK 只接受与配置 endpoint 同源的 capability URL。

## 6. 照片

### 6.1 照片图不变量

`PhotoGraph::from_records` 会拒绝以下情况（`GraphError`）：

- 有 `CPLAsset` 但其 `masterRef` 不在同一 zone；
- master 的 `photoId` 与引用它的 asset 名不一致；
- master 与 asset 的 `contentGeneration` 不一致；
- 省略 `resOriginalRes` / `resJPEGThumbRes` / `relativePath` / `thumbnailSourceOriginalSHA256`；
- `relativePath` 含绝对路径、`..`、空段或反斜杠；
- 同一 zone 内两张照片路径重复；
- 缩略图声明的源摘要不等于原图摘要；
- 存在没有被任何照片引用的 master；
- 任一记录缺少 `recordChangeTag`。

任何一条不满足都返回错误，不会产生“半张照片”。

### 6.2 创建

```rust
use sync_client::{AssetUpload, Connection, PhotoMetadata, PhotoUpload, UploadLease};

async fn publish(
    connection: &Connection,
    lease: &UploadLease,
) -> Result<(), sync_client::Error> {
    let upload = PhotoUpload::new(
        PhotoMetadata {
            relative_path: "2025/IMG_0001.HEIC".into(),
            is_favorite: false,
            asset_date_ms: Some(1_700_000_000_000),
            is_hidden: false,
        },
        AssetUpload::from_file("/photos/IMG_0001.HEIC", "image/heic")
            .await?
            .with_dimensions(4032, 3024),
        AssetUpload::from_file("/photos/IMG_0001-thumb.jpg", "image/jpeg").await?,
    );
    let photo = connection.create_photo(lease, upload).await?;
    println!(
        "created {} (master {}, generation {})",
        photo.photo_id, photo.master_id, photo.content_generation
    );
    Ok(())
}
```

`create_photo` 的顺序是：

1. 要求租约处于 `create` 阶段；
2. 一次请求预留原图与缩略图；
3. 先传原图，再传缩略图；
4. 用一个 atomic mutation 创建 master 与 asset；
5. 用响应中的两条记录重新构建并校验照片。

新照片的 `photo_id`、`master_id`、`content_generation` 由 `PhotoUpload::new` 生成。如果要导入外部已有的记录名，用 `PhotoUpload::with_ids`，但 `content_generation` 仍必须是非 nil UUID。

### 6.3 更新元数据

```rust
use sync_client::{Connection, RemotePhoto, UploadLease};

async fn rename_and_favorite(
    connection: &Connection,
    lease: &UploadLease,
    photo: &RemotePhoto,
) -> Result<RemotePhoto, sync_client::Error> {
    let mut metadata = photo.metadata();
    metadata.is_favorite = true;
    metadata.relative_path = "2025/renamed.heic".into();
    metadata.asset_date_ms = None; // None 表示移除 assetDate

    connection
        .update_photo_metadata(lease, photo, metadata)
        .await
}
```

要求 `modify` 阶段。写入使用照片当前 `assetRevision` 作为条件；同一 zone 中别的设备已经改过时返回冲突，而不是覆盖。

### 6.4 替换内容

```rust
use sync_client::{AssetUpload, Connection, PhotoContentUpdate, RemotePhoto, UploadLease};

async fn replace_content(
    connection: &Connection,
    lease: &UploadLease,
    photo: &RemotePhoto,
) -> Result<RemotePhoto, sync_client::Error> {
    let update = PhotoContentUpdate::new(
        AssetUpload::from_file("/photos/edited.heic", "image/heic").await?,
        AssetUpload::from_file("/photos/edited-thumb.jpg", "image/jpeg").await?,
    );
    connection.update_photo_content(lease, photo, update).await
}
```

要求 `modify` 阶段。每次内容替换都生成新的 `contentGeneration`，并在一个 atomic mutation 中同时更新 master（generation、缩略图源摘要、两个 descriptor）和 asset（generation），两者仍使用各自读到的 revision。`PhotoContentUpdate::new` 自动生成 generation；重试时先保存该 generation，再用 `PhotoContentUpdate::with_generation` 和相同字节重建，才能得到同一个 operation id。

### 6.5 删除

```rust
use sync_client::{Connection, RemotePhoto, UploadLease};

async fn delete(
    connection: &Connection,
    lease: &UploadLease,
    photo: &RemotePhoto,
) -> Result<(), sync_client::Error> {
    connection.delete_photo(lease, photo).await
}
```

要求 `delete` 阶段。asset 与 master 在同一个 atomic mutation 中按各自 revision 删除。

`update_photo_metadata_with_events`、`delete_photo_with_events` 接受 `EditEvents`，用于把本地编辑/删除事件写入记录的 `_syncserver`。重试时必须复用同一份事件：服务端按完整逻辑请求校验幂等键。

### 6.6 幂等与不确定结果

每个照片写操作都有确定性的 operation id：

```rust
use sync_client::{create_operation_id, Connection, OperationState, PhotoUpload, UploadLease};

async fn reconcile_create(
    connection: &Connection,
    lease: &UploadLease,
    upload: PhotoUpload,
) -> Result<(), sync_client::Error> {
    let key = create_operation_id(&upload.photo_id, &upload.content_generation);
    match connection.create_photo(lease, upload).await {
        Ok(photo) => {
            println!("created {}", photo.photo_id);
        }
        Err(_) => match connection.operation_status(&key).await? {
            OperationState::Committed(_) => { /* 已提交：按 photo id 重新读取 */ }
            OperationState::Pending(_) => { /* 仍在执行：稍后对账 */ }
            OperationState::Unknown => { /* 没有 receipt：可用相同 ids/字节重试 */ }
        },
    }
    Ok(())
}
```

- operation id 由照片 id、generation、摘要或 revision 计算，长度不超过 128 个可打印 ASCII 字符。
- 重试时必须保持 photo id、content generation 与字节完全相同（用 `PhotoUpload::with_ids` / `PhotoContentUpdate::with_generation` 重建），元数据和事件也要一致；否则同一个键对应了不同请求，服务端返回 `IDEMPOTENCY_CONFLICT`。
- 如果租约已经丢失并重新预留，资源 asset id 会变化，同一个记录仍以 photo id 为准；用 `lookup_current` 判断是否已存在，而不是盲目重写。

## 7. 错误处理

`Error` 的主要分支与恢复方式：

| 错误 | 含义 | 建议处理 |
| --- | --- | --- |
| `Endpoint` | endpoint 不是合法的同源 HTTPS 根地址 | 修正配置 |
| `Transport` | 连接、超时或响应读取失败 | `is_retryable()` 为 true 时可重试 |
| `Protocol` | 响应结构、字段或不变量不符合契约 | 停止本轮，记录日志 |
| `Authentication` | 无 token / token 被拒绝 | 重新登录或更换 token |
| `Api { status, failure }` | 非 2xx 且带错误 envelope | 按 `code()` 分类 |
| `InBand(failure)` | HTTP 200 内的错误 envelope | 逐项处理，不要当作成功 |
| `LeaseLost` | 租约已失效或被 fence | 停止写入，下一轮重新 acquire |
| `LeaseBusy` | 另一设备持有租约 | 跳过本轮 |
| `PhaseMismatch` | 阶段不允许该操作 | 检查 delete → modify → create 顺序 |
| `NotCurrent` | 本地 token 落后 | 先应用 changes，再重新 check |
| `ExpiredSyncToken` | token 超出保留范围 | 重新 bootstrap |
| `NotFound` | 记录或 operation receipt 不存在 | 区分“已删除”和“从未存在” |
| `IdempotencyConflict` | 同一键对应不同请求 | 用 `operation_status` 对账 |
| `QuotaExceeded` | 账号存储配额不足 | 停止上传，提示用户 |
| `ChecksumMismatch` / `SizeMismatch` | 本地字节与声明摘要不一致 | 重新计算摘要并重新预留 |
| `UploadRejected` | 服务端拒绝上传内容/格式 | 不重试相同字节 |
| `UploadFailed` | 上传会话终态失败 | 重新预留，不能续传 |
| `Io` | 本地文件读写失败 | 按应用策略处理 |
| `Graph(GraphError)` | 照片图不满足不变量 | 拒绝该 pair，不能落库 |

辅助方法：`status()`、`code()`、`is_lease_lost()`、`is_retryable()`、`server_record()`。服务端在冲突时可能返回 `serverRecord`，其中是服务端当前版本，可用于重新合并。

## 8. 持久化建议

建议落盘：

- 每个 zone 的 `syncToken`；
- bootstrap 期间的 `snapshotToken`（仅在 bootstrap 未完成时有意义）；
- 记录的 `recordChangeTag`（条件写入的依据）；
- 照片 identity：`photoId`、`masterId`、`contentGeneration`、`relativePath`；
- 资源 identity：`assetId`、`size`、`fileChecksum`、`checksumAlgorithm`；
- 已发出的 operation id，用于 `operation_status` 对账。

不要落盘为身份：

- `downloadURL`、`receipt`、上传 capability URL；
- bearer token（如确需持久化，应放入系统安全存储）；
- 账号密码。

## 9. 限制与约定

- 服务端协议版本必须为 1，checksum 必须为 SHA256，namespace 必须是 `production/private`。
- `publicBaseURL` 必须与配置 endpoint 同源；不支持 CDN 等独立资源源。
- 单次 JSON 响应上限 8 MiB；上传/下载传输单独使用长超时。
- 分页大小默认不超过 100，且不会超过服务端公告的 `syncLimits.maxPage`。
- 当前不支持上传断点续传，`uploadResume` 能力位不被依赖。
- 一个 `UploadLease` 只绑定一个 zone。租约是 namespace 级互斥，不能同时持有两个 zone 的租约；切换 zone 必须先释放再重新获取。
- SDK 不执行同步调度，也不把“本地删除”自动解释为云端删除。

## 10. 示例与验证

仓库内示例：

```sh
SYNC_ENDPOINT=https://sync.example.com \
SYNC_USERNAME=alice SYNC_PASSWORD=secret \
cargo run -p sync-client --example read_sync -- /tmp/photos

SYNC_ENDPOINT=https://sync.example.com \
SYNC_USERNAME=alice SYNC_PASSWORD=secret \
cargo run -p sync-client --example upload_photo -- \
    /photos/IMG_0001.HEIC /photos/IMG_0001-thumb.jpg 2025/IMG_0001.HEIC
```

验证命令：

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --all-targets
cargo test --locked --all-targets
```

集成测试 `sdk/tests/integration.rs` 使用本地 mock 服务覆盖：登录/登出、能力探测、bootstrap 分页、changes、lookup、租约检查与阶段、双资源预留、上传摘要校验、照片创建、元数据更新、内容替换、删除、下载与 operation receipt。
