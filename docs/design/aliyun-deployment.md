# 阿里云首次部署与旧 WebDAV 清理

日期：2026-10-15。用户授权通过 `ssh aliyun` 部署 SyncServer，并删除旧 WebDAV 同步数据以释放空间。本记录对应真实服务器操作，不是部署模板或待执行计划。

## 已部署服务

| 项目 | 已核实状态 |
|---|---|
| 公网入口 | `https://47.103.11.237`，HTTPS 证书校验通过 |
| 运行环境 | Ubuntu 24.04.4 LTS，x86_64 |
| 运行方式 | `syncserver.service`，独立 syncserver 用户，active/running、开机启用 |
| 二进制 | `/usr/local/bin/syncserver`，root 拥有、0755 |
| 后端监听 | `127.0.0.1:8790`，由 Nginx 443 转发 |
| 配置 | `/etc/syncserver/server.env` |
| 数据 | `/var/lib/syncserver`，syncserver 拥有、0700 |
| 凭证 | `/etc/syncserver/api.key` 与 `signing.key`，syncserver 拥有、0600，未写入仓库或报告 |
| namespace | container=`com.apple.photos.cloud`、production/private、zone=`PrimarySync` |
| Nginx | `/etc/nginx/sites-available/syncserver`，已启用；使用[仓库模板](../../deploy/nginx.conf.example) |
| 运行检查 | `/usr/local/libexec/syncserver/smoke.py` |
| 构建清单 | `/etc/syncserver/deployment.json`，记录源码/二进制摘要与构建方式 |

复用既有 Let's Encrypt IP 证书 lineage `/etc/letsencrypt/live/webdav-ip`。保留 HTTP ACME webroot，当前证书有效期至 2026-10-19，续期 timer 为 active；真实续期模拟和 deploy hook 成功。活动钩子现只校验并重载 Nginx，旧 WebDAV 重启命令已移除；旧钩子备份移至 `/var/backups/syncserver-transition/20261015T033118Z`，不留在活动钩子目录。

## 构建来源

使用本地 `rust:1.88-bookworm` / `debian:bookworm-slim` 构建镜像中的 release 二进制，在 Ubuntu 上通过 systemd 直接运行；生产服务器不需要 Rust 工具链或容器运行时。

- 基础提交：`2d9f9a74a4168f03f4079537c3336e75cbb0c069`。
- 部署构建时包含尚未提交的本地 M5 运维指标实现；后续源码交付以 Git 记录为准。
- 源码 SHA-256：`91ae1fbee045a339e0e278b4fb5428f1ef6dbd406dd8810eadfbae4cb8db5539`。
- 二进制 SHA-256：`b9b9ee185954cb2a8f6d26f8f7bc37f0b60869ade77f87e5c5a69d5aee5aa8ed`，上传前与服务器安装后相同。
- 本地构建镜像：`localhost/syncserver:aliyun-20261015`。

源码摘要按路径排序，涵盖 Cargo.toml、Cargo.lock、src 下全部 .rs/.sql；依次拼接相对路径、NUL、文件字节、NUL 后计算。服务器清单和本地忽略目录 `local-run/aliyun-deploy` 保存构建标识；这不是已推送新提交或新代码远端 CI 的证据。

## 已授权的数据清理

核实 rclone 的真实服务根目录是 `/srv/webdav`，它不是符号链接或独立挂载，内部没有符号链接或嵌套挂载。停止并禁用 `webdav.service` 后，仅清理该目录内容，保留目录及原有小型配置；没有删除其他服务数据。

- 删除文件：3,544 个。
- 逻辑总大小：20,606,793,593 字节，约 19.2 GiB。
- 已分配总大小：20,614,037,504 字节。
- 清理后 `/srv/webdav` 剩余条目：0。
- 根磁盘：使用率从 74% 降到 22%，可用空间从约 10 GiB 增到约 30 GiB。
- WebDAV 最终状态：inactive / disabled；443 由 Nginx 提供 SyncServer。

旧数据已删除，没有执行 WebDAV 到原生 SyncServer 的数据迁移。PhotoViewer 的本地照片未修改；客户端 C1 仍未接入，原 WebDAV 任务不能直接使用这个原生协议入口。

## 实机验证

| Tests: 实际执行的检查 | 结果 |
|---|---|
| `podman build --pull=never -f deploy/Dockerfile -t localhost/syncserver:aliyun-20261015 .` | PASS，Rust 1.88.0 release 构建 |
| 服务器 `systemd-analyze verify /etc/systemd/system/syncserver.service` | PASS |
| 服务器 `nginx -t` 与 reload | PASS |
| 本地及服务器通过公网 HTTPS 执行 `tools/smoke.py` | PASS，健康/就绪/认证能力/监控/快照/变化 |
| 公网合成双资源验收 | PASS，真实 HTTP/1.1 分块上传、SHA-256、会话 Published、GET/HEAD/Range/304 |
| 公网租约与照片对验收 | PASS，租约竞争拒绝、阶段转换、atomic 成对创建/lookup/条件成对删除 |
| 公网空变化及 HEAD 成本 | PASS，连续 5 轮没有增加正文读取/哈希计数 |
| `systemctl restart syncserver.service` 后检查 | PASS，generation 不变、旧同步 token 继续有效、runId 改变 |
| `certbot renew --cert-name webdav-ip --dry-run --run-deploy-hooks` | PASS，续期模拟和实际 deploy hook 成功 |
| 运行中 SQLite 只读 `quick_check` / `foreign_key_check` | PASS，ok / 0 条失败 |
| 最终数据及权限清点 | PASS，0 条活跃记录、暂存目录为空、两个合成对象共 140 字节，凭证 0600 |

双资源测试的照片记录已成对删除。首版没有资产 GC，因此保留两份共 140 字节的合成对象及少量会话/墓碑历史；没有导入用户照片。

上一步 M5 实现的最终本地 Rust 闸门已通过，63 个测试；本次部署没有修改 Rust 源码。初次部署验收时尚未提交或推送 Git，新代码的远端 CI 当时未运行；后续提交、推送与 CI 以 Git 交付记录为准。没有重启整台服务器，也没有将本次首次部署当作大规模性能、备份恢复演练或 C1 的验收。

## 连接与日常检查

用户机器上连接说明保存在 `/home/luyao/.config/syncserver/aliyun/connection.json`，API key 保存在同目录 `api.key`；目录 0700、文件 0600。签名私钥保留在服务器，不下发客户端。

```sh
python tools/smoke.py https://47.103.11.237 --api-key-file /home/luyao/.config/syncserver/aliyun/api.key
ssh aliyun 'sudo systemctl status syncserver.service --no-pager'
ssh aliyun 'sudo journalctl -u syncserver.service -n 50 --no-pager'
```

后续仍需补齐自动化备份恢复工具、3 万记录基准及真实客户端接入；修改签名密钥或恢复旧数据库前遵守[运维约束](security-operations.md)。
