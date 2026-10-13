# 安全、配置与部署

状态：M1/M2 已实现配置、文件权限、Bearer、记录范围与数据目录锁；部署样例和同步凭证发行仍待后续阶段。

## 首版身份

一个账户，Bearer API key 对应配置的 namespace。API key 和签名密钥从权限受限的文件读取；不内置默认凭证，不放入 Git，也不输出明文到日志。

保留 tenant/container/environment/database 的隔离边界，但不宣称多用户权限系统已完成。`shared/public` 请求明确拒绝。

## token 和 URL

使用经过验证的 HMAC 实现，游标、上传能力、下载能力使用不同域。校验种类、namespace、zone、generation、查询参数、位置、方法和到期时间。

签名 URL 是凭证，不记录完整 query、不进入错误正文。下载 URL 使用配置的 public base URL，不能相信未校验的 Host 或任意代理头。

服务端 SHA-256 证明实际字节身份，不自动证明客户端有权访问另一个 namespace 的资产。

## 配置

| 配置 | 含义 |
|---|---|
| SYNC_BIND | 默认 127.0.0.1:8790，明确配置后才对外暴露 |
| SYNC_DATA_DIR | 必填；数据库、对象、暂存所在根目录，单进程目录锁 |
| SYNC_PUBLIC_BASE_URL | 默认 http://127.0.0.1:8790；HTTP 只允许 literal loopback IP（不接受 DNS 主机名），其他必须 HTTPS；基址不能有用户信息、query/fragment 或路径前缀 |
| SYNC_API_KEY_FILE | 请求认证密钥文件 |
| SYNC_SIGNING_KEY_FILE | 跨重启稳定的签名密钥文件 |
| SYNC_CONTAINER | 默认 com.apple.photos.cloud；已有数据目录不得静默更改 |
| SYNC_ZONES | 部署初始化的 zone 清单，默认 PrimarySync |
| SYNC_MAX_ASSET_BYTES | 默认 536870912（512 MiB），可正数配置 |
| SYNC_THUMBNAIL_MAX_BYTES | 默认 2097152，v0.1 不可超过 2 MiB |
| SYNC_UPLOAD_CONCURRENCY | 默认 2，必须正整数 |
| SYNC_DOWNLOAD_CONCURRENCY | 默认 8，必须正整数 |
| SYNC_UPLOAD_IDLE_TIMEOUT_SECS | 默认 30，必须正整数 |
| SYNC_UPLOAD_TOTAL_TIMEOUT_SECS | 默认 3600，必须正整数 |
| SYNC_DB_QUEUE_CAPACITY | 默认 128，必须正整数 |
| SYNC_JSON_BODY_LIMIT | 默认 1048576（1 MiB），必须正整数 |

数值和 URL 在启动时校验。SYNC_BIND 非 loopback 时 public base 必须为 HTTPS，不能使用默认 HTTP 回环地址充当公开配置。没有密钥、不可写目录或公开 URL 错误时启动失败，不降级为无认证服务。上述 v1 默认值已固定，M1 配置测试和能力声明返回实际配置；变更需同步契约。namespace 与 zone 集合在初始化后固定，顺序调整不算集合变化；不一致按 CONFIG_SCOPE_MISMATCH 拒绝启动。

签名密钥文件为单行标准 Base64，解码恰好 32 随机字节；API key 为单行不透明 ASCII（32–512 字符），建议由 32 随机字节的 Base64URL 生成。Unix 文件仅当前服务用户可读写（0600），不把密钥写入例子。

所有凭证采用版本化、域分离 HMAC-SHA256；payload 的 Base64URL 无 padding。MAC 绑定实际编码 payload 与种类域，校验后再解码检查语义。快照/分页/增量 token 和资产 receipt 首版没有时间到期，受 generation/签名密钥约束；上传/下载能力包含 expires，期限 15 分钟。不得用一种凭证授权另一种操作。

上传 URL 到期只限制新请求；已接受的 Receiving 仍受空闲/总时长上限。已进入发布意图的操作不因凭证到期取消。会话 TTL 24 小时，不回收活跃接收或恢复中的意图。

## 部署

首版单进程、单写入所有者，不允许多进程同时管理同一数据目录。公网入口使用 TLS 反向代理或后续验证的内置 TLS；不能把“支持 Bearer”当成传输加密。

systemd 使用独立用户、受限目录和明确的关闭超时。容器只挂载受管理数据目录及只读密钥。部署样例在 M5 编写，不提前给出不能运行的命令。

## 备份与恢复

运行中元数据备份使用 SQLite 备份机制，不随意复制 WAL 文件。数据备份包含不可变资产、数据库和 generation/签名配置；备份清单验证引用闭包。

恢复备份后显式更新 database_generation，让旧游标重建清单；常规重启不更新。签名密钥轮换会使旧能力失效，需要有明确维护流程。

## 可观察性

日志包含 request_id、范围和错误类别，不包含密钥、原始私密字段或完整能力 URL。路径和 recordName 可能包含个人信息，默认避免输出完整值。

健康、就绪、队列长度、上传字节、哈希字节、变化页数和恢复失败计数须可观测。v0.1 不把 metrics 地址当作同步 API。

## M1 的实现边界

M1 的文件权限要求为数据目录 0700、凭证及管理文件 0600，服务用户拥有，拒绝符号链接和文件硬链接。配置文件中的队列/并发/正文限额须能表示为 32 位无符号正整数；资产大小不得超出 SQLite 的有符号 64 位范围，总上传超时不得小于空闲超时。

初始化持久化域分离 HMAC 签名密钥指纹；重启时不一致返回 SIGNING_KEY_MISMATCH，避免意外密钥替换导致未来 token 失效。M1 不提供在线轮换或备份恢复 generation 重置工具；后续工具需独立验证。API key 可在停止服务后更换，不改变数据库 generation。签名密钥与 API key 不允许复用同一内容。

当前配置的上传/下载限额是 M4 的保留配置。M1 已生效的是元数据正文限制、30 秒 JSON 接收上限、数据库队列和受认证请求并发门槛；并发门槛使用 dbQueueCapacity，饱和返回 503，不能将保留配置当成已实现资产传输。

## M2 请求与结果边界

严格 JSON 拒绝重复对象键；Idempotency-Key 拒绝重复头、空白和超长值。修改/当前读取最多 400 项，每记录合并字段 JSON 最多 1 MiB，每响应/事务工作集有 8 MiB 预算。通用引用仅同 zone/action=NONE，不触发外部读取或级联删除。业务冲突允许返回本账户的当前记录；内部错误不泄漏 SQL、磁盘路径或故障注入文本。

## 上行租约新增配置与安全（待实现）

计划增加 SYNC_UPLOAD_LEASE_TTL_SECS（默认 90）和 SYNC_UPLOAD_LEASE_HEARTBEAT_SECS（默认 30，须满足 `0 < heartbeat <= ttl/3`），返回实际配置并暴露 lease/fencing/progress-gate/phase capability。M2 尚无这两个配置或能力，不把计划值当成运行结果。

leaseToken 是秘密，与 Bearer/资源凭证不同域；deviceId 不足以授权。所有变更请求都校验租约/固定范围/阶段，控制路径同样防重复 header 和越权。上传能力绑定租约代次，URL 15 分钟有效并不授予已过期租约写入权。读取和结果状态查询仍只需正常读取认证。

服务器按单调时钟过期，正常重启推进独立持久 fence 并失效租约，保留元数据同步 generation。客户端用成功续租请求的单调起始时刻与已知 TTL 计算保守截止时间，不能把晚到响应接收时刻当成新 90 秒、不能自行延长未确认授权。

已知期限内续租网络错误可以有界重试；服务器明确失锁或保守截止到期令本轮终止，所有未完成项报错。日志只含错误类别/关联 ID，不包含 leaseToken 或上传能力 URL。
