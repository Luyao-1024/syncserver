# 部署与检查

2026-10-15 已完成一次[阿里云真实部署](../docs/design/aliyun-deployment.md)，包含公网 TLS、续期模拟和资源往返验证；该记录对应具体实例与构建来源。

本目录提供首次部署配置；不自动修改原 WebDAV 或用户云服务器。基本实现/验证范围见[服务端说明](../docs/design/v0.1-server.md)。服务启动必须持有数据目录独占锁，只允许一个进程管理一个目录。

本地仓库的开发与 release 启动/重启统一使用 `./start.sh dev` / `./start.sh release`，见 [本地启动说明](../README.md)。以下 systemd/容器为独立部署入口，其实例由对应服务管理器重启。

## systemd

1. 在目标 Linux 主机 `cargo build --release --locked --bin syncserver`，安装二进制至 `/usr/local/bin/syncserver`。
2. 安装 `syncserver.sysusers` 至 `/etc/sysusers.d/syncserver.conf`，执行 `systemd-sysusers` 创建独立服务用户。
3. 创建 `/etc/syncserver`（服务用户可读取），将 `server.env.example` 复制为 `server.env`，填写真实 HTTPS public base；反向代理转发到 loopback 8790，保留上传/下载流，不缓存 JSON 或能力 URL。
4. 生成 32 字节 Base64 签名密钥文件，必须由 syncserver 用户拥有、0600；不写到 Git、不将值放入命令行。首次发布后签名密钥需要稳定保留。账户部署不需要 API key：省略 `SYNC_API_KEY_FILE`，用 `syncserver users create` 建立第一个账户；只有尚未注册任何账户的旧单账户部署才独立生成 0600 的 API key 文件，一旦出现账户该键即停止授权。
5. 安装 unit 至 `/etc/systemd/system/syncserver.service`，执行 daemon-reload，再 enable/start。StateDirectory 管理数据目录 0700；工作目录不能与其他实例共享。
6. 运行只读 smoke。已创建用户的服务用账户登录，口令从受保护输入读取、不进命令行：`python3 tools/smoke.py https://你的服务地址 --username 账户名 --password-stdin`（把口令通过管道或终端输入）。没有用户的旧单账户部署继续用 `--api-key-file /安全路径/api.key`。两个参数互斥且必须提供其一。

unit 的关闭等待 75 秒。客户端可能收到大文件中断，按持久会话查询并创建新会话重传；SIGKILL 的意图恢复规则已由服务端测试覆盖。不把强制终止的 HTTP 请求当作成功。

## 容器

仓库根目录执行 `podman build -f deploy/Dockerfile -t syncserver:local .`。镜像使用 UID/GID 10001；准备可由该用户读写的 0700 数据目录及其拥有的 0600 密钥，作为独立卷及只读凭证挂载。rootless 用户 ID 映射需按目标主机配置；不自动 chmod/chown 用户的真实目录。

容器监听 0.0.0.0:8790，必须显式配置 HTTPS `SYNC_PUBLIC_BASE_URL` 及凭证文件路径。在主机上只映射到 loopback，再由 TLS 反向代理服务。镜像本身不携带证书、凭证或数据，也不自动发布到镜像仓库。

## 备份与恢复

正式部署后备份包含 SQLite、不可变对象、签名密钥和固定 scope 配置。首次版本不提供在线备份 API；初版可先正常停止服务，再完整备份整个私有数据目录，避免复制仍活动的 WAL。保留权限及引用闭包，备份作为敏感数据存储。

恢复旧备份必须显式更换 database_generation，防止旧 token 跳过恢复范围；正常重启不得换 generation。当前未正式发布，开发版数据库格式兼容不承诺。实际公网部署、容器构建、systemd 启动和远端 CI 各自记录验证结果，不以本地 HTTP 测试替代。


## 运维监控

`GET /v1/metrics` 使用同一 Bearer API key，按[指标契约](../docs/contracts/metrics.md)返回 JSON；反向代理保持 no-store，不记录 Authorization。独立监控配额不占租约控制/普通请求配额，采样不提交数据库工作。进程重启改变 runId 并重置累计计数，采集器须检测重置；ready=false 表示服务不接收同步任务。`/readyz` 另行验证数据库 ping，两个信号需分别解释。

只读 smoke 验证此能力、版本、运行标识、两个队列配置以及各 zone 变化页计数；它不上传/删除资产，不替代公网 TLS、systemd 或容器实机验证。


## 日志与留存

unit 与容器都不重定向标准输出：控制台日志由 journald（`journalctl -u syncserver`）或容器日志驱动负责留存与轮转，服务端不代为配置轮转，也不读取自己的 stdout。`SYNC_LOG_FORMAT=json` 让 stdout 成为逐行 JSON，便于日志驱动采集；`systemd-journald` 的保留空间需按主机情况自行确认，默认上限可能早于服务端阈值就丢弃 stdout。

`logs.sqlite` 落在数据目录内（systemd 下即 `/var/lib/syncserver`），随 StateDirectory 的 0700 权限受保护，`ProtectSystem=strict` 不影响其写入。默认阈值是一天：窗口内的日志原样保存且**不设条数或字节上限**，超过 `SYNC_LOG_RETENTION_RAW_SECS` 的记录压缩为 gzip 段，压缩总量不超过 `SYNC_LOG_ARCHIVE_MAX_BYTES`（默认 1 GiB），超限时先整段删除最旧段。磁盘水位告警须把未压缩窗口计入：突发调试或长时间高请求量都会让当天日志显著增长，可通过 `GET /v1/metrics` 的 `metrics.logging.rawRows`/`archiveBytes` 观察。

排查用 `syncserver logs`（只读，不取数据目录锁，服务运行时可用）：

```bash
sudo -u syncserver sh -c 'set -a; . /etc/syncserver/server.env; set +a; syncserver logs --level warn --since 24h'
sudo -u syncserver sh -c 'set -a; . /etc/syncserver/server.env; set +a; syncserver logs --request-id ID --archive --json'
```

命令与其他子命令一样从环境变量读取配置，至少需要 `SYNC_DATA_DIR` 与 `SYNC_SIGNING_KEY_FILE` 存在（后者只参与配置校验，命令不读取其内容），因此上面先载入 `server.env`。日志包含请求路径、错误类别与（开启 payload 时）记录名等运维信息，属敏感数据：备份须按敏感数据保留，不导出到不受控位置，也不长期开启 `SYNC_LOG_PAYLOAD`。契约与等级策略见[日志与留存契约](../docs/contracts/logging.md)。


## Nginx HTTPS 反向代理

[可运行配置模板](nginx.conf.example)转发到 loopback 8790，部署时替换 server_name 和证书路径。默认接收上限 512 MiB 与服务端默认一致；改变应用资源上限时一并调整。上传和下载关闭代理缓冲，HTTP/1.1 保留分块流，变更请求不自动重试；capability URL 带秘密查询参数，访问日志关闭。安装后先 `nginx -t`，再 reload。

证书续期继续使用原 HTTP ACME webroot；部署钩子校验并 reload Nginx。替换旧服务时检查所有活动续期钩子，避免钩子重启已退役服务。凭证文件只放权限受限目录，不写入仓库或部署报告。


## 用户空间上限

环境模板显式设置 `SYNC_USER_STORAGE_QUOTA_BYTES=20000000000`（每用户 20 GB，十进制）；缺省也使用此值，修改后重启服务。用户用现有登录 Bearer 请求 `GET /v1/storage` 查询总额/已用/预留/可用空间。该配置限制每用户全部 zone 的资源总额，不替代单文件或 Nginx 请求正文上限，也不限制 SQLite、日志和暂存的磁盘开销。按当前文件的逻辑大小计量，相同内容的多个独立文件分别计费；删除释放逻辑配额，历史及未引用对象不计入用户已用空间，见[完整契约](../docs/contracts/storage-quota.md)。本地模板变更不代表线上已经部署。


## 2026-10-23 本机隔离服务与 frp

当前照片后端以本机 `syncserver-local.service` 运行；阿里云只保留 Nginx/frp。使用专用服务用户、只读程序/密钥、唯一可写数据目录及最大 64 GiB 的独立稀疏数据卷；日志 SQLite、元数据、认证库和 journald 均有物理预算。公网只复用原有 HTTPS 443，7000/8790 只绑定阿里云回环。

重启已安装版本使用 `systemctl restart syncserver-local.service`；源码更新需重新构建/安装 release。具体数据位置、回滚边界、实际验证和密码交付位置见[安全修复实施记录](../docs/design/security-remediation.md)。本机开发仍用 start.sh，systemd 安装服务使用自身生命周期管理。
