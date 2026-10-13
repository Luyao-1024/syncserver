# 安全、配置与部署

状态：部署契约，尚无实际服务或配置文件。

## 首版身份

一个账户，Bearer API key 对应配置的 namespace。API key 和签名密钥从权限受限的文件读取；不内置默认凭证，不放入 Git，也不输出明文到日志。

保留 tenant/container/environment/database 的隔离边界，但不宣称多用户权限系统已完成。`shared/public` 请求明确拒绝。

## token 和 URL

使用经过验证的 HMAC 实现，游标、上传能力、下载能力使用不同域。校验种类、namespace、zone、generation、查询参数、位置、方法和到期时间。

签名 URL 是凭证，不记录完整 query、不进入错误正文。下载 URL 使用配置的 public base URL，不能相信未校验的 Host 或任意代理头。

服务端 SHA-256 证明实际字节身份，不自动证明客户端有权访问另一个 namespace 的资产。

## 计划配置

| 配置 | 含义 |
|---|---|
| SYNC_BIND | 默认仅本机监听，明确配置后才对外暴露 |
| SYNC_DATA_DIR | 数据库、对象、暂存所在根目录 |
| SYNC_PUBLIC_BASE_URL | 签名资源 URL 的外部基址 |
| SYNC_API_KEY_FILE | 请求认证密钥文件 |
| SYNC_SIGNING_KEY_FILE | 跨重启稳定的签名密钥文件 |
| SYNC_CONTAINER | 唯一开放的 CloudKit 风格 container |
| SYNC_ZONES | 部署初始化的 zone 清单，默认 PrimarySync |
| SYNC_MAX_ASSET_BYTES | 单资源大小上限 |
| SYNC_UPLOAD_CONCURRENCY | 并发上传上限 |
| SYNC_DB_QUEUE_CAPACITY | 数据库队列上限 |
| SYNC_JSON_BODY_LIMIT | 元数据请求大小上限 |

数值和 URL 在启动时校验。没有密钥、不可写目录或公开 URL 错误时启动失败，不降级为无认证服务。正式默认值在实现和资源测试后确定。

## 部署

首版单进程、单写入所有者，不允许多进程同时管理同一数据目录。公网入口使用 TLS 反向代理或后续验证的内置 TLS；不能把“支持 Bearer”当成传输加密。

systemd 使用独立用户、受限目录和明确的关闭超时。容器只挂载受管理数据目录及只读密钥。部署样例在 M5 编写，不提前给出不能运行的命令。

## 备份与恢复

运行中元数据备份使用 SQLite 备份机制，不随意复制 WAL 文件。数据备份包含不可变资产、数据库和 generation/签名配置；备份清单验证引用闭包。

恢复备份后显式更新 database_generation，让旧游标重建清单；常规重启不更新。签名密钥轮换会使旧能力失效，需要有明确维护流程。

## 可观察性

日志包含 request_id、范围和错误类别，不包含密钥、原始私密字段或完整能力 URL。路径和 recordName 可能包含个人信息，默认避免输出完整值。

健康、就绪、队列长度、上传字节、哈希字节、变化页数和恢复失败计数须可观测。v0.1 不把 metrics 地址当作同步 API。
