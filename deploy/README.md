# 部署与检查

本目录提供首次部署配置；不自动修改原 WebDAV 或用户云服务器。基本实现/验证范围见[服务端说明](../docs/design/v0.1-server.md)。服务启动必须持有数据目录独占锁，只允许一个进程管理一个目录。

## systemd

1. 在目标 Linux 主机 `cargo build --release --locked --bin syncserver`，安装二进制至 `/usr/local/bin/syncserver`。
2. 安装 `syncserver.sysusers` 至 `/etc/sysusers.d/syncserver.conf`，执行 `systemd-sysusers` 创建独立服务用户。
3. 创建 `/etc/syncserver`（服务用户可读取），将 `server.env.example` 复制为 `server.env`，填写真实 HTTPS public base；反向代理转发到 loopback 8790，保留上传/下载流，不缓存 JSON 或能力 URL。
4. 独立生成 API key 与 32 字节 Base64 签名密钥文件，必须由 syncserver 用户拥有、0600；不写到 Git、不将值放入命令行。首次发布后签名密钥需要稳定保留。
5. 安装 unit 至 `/etc/systemd/system/syncserver.service`，执行 daemon-reload，再 enable/start。StateDirectory 管理数据目录 0700；工作目录不能与其他实例共享。
6. 运行只读 smoke：`python3 tools/smoke.py https://你的服务地址 --api-key-file /安全路径/api.key`。

unit 的关闭等待 75 秒。客户端可能收到大文件中断，按持久会话查询并创建新会话重传；SIGKILL 的意图恢复规则已由服务端测试覆盖。不把强制终止的 HTTP 请求当作成功。

## 容器

仓库根目录执行 `podman build -f deploy/Dockerfile -t syncserver:local .`。镜像使用 UID/GID 10001；准备可由该用户读写的 0700 数据目录及其拥有的 0600 密钥，作为独立卷及只读凭证挂载。rootless 用户 ID 映射需按目标主机配置；不自动 chmod/chown 用户的真实目录。

容器监听 0.0.0.0:8790，必须显式配置 HTTPS `SYNC_PUBLIC_BASE_URL` 及凭证文件路径。在主机上只映射到 loopback，再由 TLS 反向代理服务。镜像本身不携带证书、凭证或数据，也不自动发布到镜像仓库。

## 备份与恢复

正式部署后备份包含 SQLite、不可变对象、签名密钥和固定 scope 配置。首次版本不提供在线备份 API；初版可先正常停止服务，再完整备份整个私有数据目录，避免复制仍活动的 WAL。保留权限及引用闭包，备份作为敏感数据存储。

恢复旧备份必须显式更换 database_generation，防止旧 token 跳过恢复范围；正常重启不得换 generation。当前未正式发布，开发版数据库格式兼容不承诺。实际公网部署、容器构建、systemd 启动和远端 CI 各自记录验证结果，不以本地 HTTP 测试替代。
