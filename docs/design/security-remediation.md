# 公网安全修复与本机重新部署

日期：2026-10-23。状态：本机实现、完整验证及真实公网重新部署已完成。提交/推送和远端 CI 结果以对应 Git 提交及其工作流记录为准，以下部署验证不代替远端 CI。照片后端继续留在本机，阿里云仅提供 Nginx/frp 入口。

## 修复

- 管理员权限不再从 Store 缓存读取。缓存只保存不可变账户 ID，每次请求使用认证目录返回的最新 User；管理员降权后旧、新会话都不能继续访问管理 API。已缓存普通账户的提升也即时生效，且不能继续访问照片接口。
- 网页退出显式使用退出前保存的 Bearer 调用撤销 API。源码客户端通过真实公网退出后，旧令牌访问返回 401；发行 JS 与本地构建逐字节一致。
- 登录正文接收与 Argon2 计算分开，分别最多 16/2 并行；两条慢正文不再占据所有密码计算名额。
- 登录按来源 IP 有界限流，失败计数按来源与用户名组合；账户的失败计数不再阻断正确密码。默认忽略代理头，仅从显式可信代理读取单个合法来源 IP；公网 Nginx 覆写 X-Forwarded-For。
- 增加独立于逻辑配额的物理资源预算：默认每账户 40 GB，覆盖未引用/历史/隔离对象、接收与发布准备、有效上传预留。超额返回 507；保留 1 GiB 文件系统余量，正文开始前重新检查。不裁剪历史或删除照片，不引入 GC，不重新 hash 正文。
- SQLite 增长有物理包络：日志默认 256 MiB、每账户元数据 1 GiB、认证目录 64 MiB；日志达到上限时拒绝新增落库并计数，保留已有近期记录。WAL 定期 checkpoint，最终总量再由独立数据卷硬限制。契约已同步，不继续承诺磁盘无限增长。
- 管理员默认会话从 30 天缩短到 2 小时；提升角色和打开旧认证库也收紧既有管理员会话。实际 admin 临时密码已轮换为随机密码，旧会话撤销；新密码只保存在用户拥有的 0600 私有交付文件中。普通账户密码与会话未被重置。没有新增 MFA。
- 更新 rustls-webpki 至 0.103.16、webpki-roots 至 1.0.10，移除此前命中的开发依赖公告。

## 回归验证

Tests:

- PASS `cargo fmt --all --check`。
- PASS `cargo clippy --locked --all-targets -- -D warnings`。
- PASS `cargo build --locked --all-targets`。
- PASS `cargo test --locked --all-targets`：154 个测试、0 失败（含 SDK 与 5 个新增 security_http 场景）。
- PASS `cargo build --locked --release --bin syncserver`。
- PASS `npm test`（web）：真实 HTTP 会话退出测试。
- PASS `npm run build`、`npm run lint`（web）。
- PASS `python3 tools/check-docs.py`、`git diff --check`。
- PASS 当前 Cargo.lock 的 209 个包通过 OSV 查询，无匹配公告；官方 npm registry 审计为 0 已知漏洞。

安全回归覆盖：缓存后的降权/提升与新旧会话、两条慢正文、正确密码不受其他失败计数锁定、可信代理和伪造来源头、未引用资源上限、重启保持物理保护、并发预留不超售、失败正文释放容量、磁盘余量拒绝上传、日志达到容量上限而不裁剪已有近期记录。

## 真实部署

本机 `syncserver-local.service` 于 2026-10-23 02:53（Asia/Hong_Kong）上线，以专用无登录用户 syncserver-local 运行。程序安装在 `/usr/local/libexec/syncserver-local/syncserver`，由 root 拥有；与本地 release 构建的 SHA-256 相同。

- `ProtectHome=yes`、`ProtectSystem=strict`、NoNewPrivileges、空能力集、私有 tmp/dev、受限地址族；网络仅允许回环。
- 唯一可写服务路径为 `/var/lib/syncserver-data/data`；签名密钥只读，数据目录/文件保持 0700/0600。
- 64 GiB 稀疏 ext4 数据卷挂载于 `/var/lib/syncserver-data`，nodev/nosuid/noexec，按实际写入占用宿主磁盘；硬边界覆盖数据、SQLite 与 WAL。
- 应用日志输出进入独立 journald namespace `syncserver-local`，系统日志最多 128 MiB、运行时最多 32 MiB、单文件 16 MiB、保留最多 7 天，并有速率限制；原后台 stdout 文件已移入回滚目录，不再持续增长。
- systemd 服务和数据卷已启用开机启动及失败重启；本机 frpc 继续由用户服务管理，设置登录自启与自动重连。未修改用户 linger。

迁移使用停服务后的一致复制；账户 UUID、用户名/角色、各数据库 generation、records/record_versions/changes/assets 数量在迁移前后相同。保留 root-only 回滚目录：`/var/lib/syncserver-rollback/20261023-025306`。这份副本保留原密码版本，仅供受控回滚，不应直接恢复成公网长期状态。

公网地址仍为 `https://47.103.11.237/console/`。只复用 443，frps 的 7000/8790 继续只监听阿里云回环。真实公网登录、2 小时管理员期限、网页退出、旧令牌拒绝、发行 bundle 对比及实际数据卷容量检查均通过；应用当前运行用户和沙箱配置已通过 systemd 状态核查。

新管理员密码在 `/home/luyao/.config/syncserver-local/admin-password`（luyao 拥有，0600）。本记录、Git 和日志均不包含其明文。

## 运维

- 状态：`systemctl status syncserver-local.service`。
- 重启已安装版本：`systemctl restart syncserver-local.service`（需要管理员认证）。
- 日志：`journalctl --namespace=syncserver-local -u syncserver-local.service`。
- frp：`systemctl --user status syncserver-frpc.service`。
- `local-run/start-public-server.sh` 现在调用 systemd 重启已安装版本。修改源码后应重新构建并安装 release，再重启此服务；开发入口 start.sh 仍用于独立开发实例，不再管理已安装 systemd 服务。

证据：忽略目录 target 下的 `security-final-{fmt,clippy,build,tests,release,docs,osv}.log/json`、`security-web-{tests,build,lint}.log`、`security-public-verification.log`，以及 local-run 中的 `security-deployment.json` / `security-public-verification.json`。未运行远端 CI、MSRV 独立验证、大规模压测或真实照片客户端 UI 全旅程，不能将本轮结果替代这些范围。
