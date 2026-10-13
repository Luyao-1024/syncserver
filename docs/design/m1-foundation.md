# M1 启动与元数据基础

日期：2026-10-13。实现范围为第一阶段，不是完整 v0.1 同步服务。

## 已建立的结构

单个 Cargo crate，Rust edition 2024，最低版本声明 1.88。Cargo.lock 固定本次依赖解析；本机验证工具链为 rustc/cargo 1.98.0。Tokio/Axum 提供异步 HTTP，SQLite bundled 构建避免系统库版本不一致。

| 路径 | 职责 |
|---|---|
| `src/main.rs`、`src/lib.rs` | 启动、服务监听、SIGINT/SIGTERM、关闭编排 |
| `src/config.rs`、`src/credentials.rs` | 确定性配置校验、私有文件读取、认证摘要和签名密钥指纹 |
| `src/api/` | Bearer、基础接口、zone 列表、能力和错误适配 |
| `src/domain/` | 与框架无关的命名空间类型 |
| `src/store/` | 私有目录锁、schema v1、WAL、有界 SQLite 单线程执行器 |
| `src/jobs/` | 元数据检查、持久启动登记、正常停止和中断启动结算 |
| `tests/` | HTTP、权限、数据库持久化、队列及真实进程测试 |

assets 目录暂只有规则；M1 没有照片记录、资产、游标或变化表。

## 基础接口

| 接口 | 认证 | 行为 |
|---|---|---|
| GET /healthz | 无 | 200，进程存活 |
| GET /readyz | 无 | 恢复完成且数据库能响应为 200，关闭/不可用/饱和为 503 |
| GET /v1/capabilities | Bearer | 阶段、实际配置、支持位、databaseGeneration、恢复和队列计数 |
| POST /database/1/{container}/production/private/zones/list | Bearer | 空 JSON 对象，返回已初始化 zoneID 数组 |

能力声明中 zoneListing 为 true；recordCrud/snapshots/incrementalChanges/assetUpload/assetDownload/appleAuthentication/sharing/uploadResume 为 false。上传/下载限额是后续阶段的保留配置，不能当成已启用接口。

签名密钥当前用于持久化身份检查，不发行签名游标或资源能力凭证。Bearer key 可离线更换；数据库 generation 不变。现有目录的签名密钥、命名空间、zone 集合不一致会拒绝启动。

## 运行与关闭

运行命令和本地测试凭证初始化见 [README](../../README.md)。启动顺序是配置 → 私有凭证 → 目录锁 → 数据库识别与事务迁移 → 范围/密钥检查 → 元数据恢复 → HTTP 监听。默认 loopback，公网 HTTPS 仍需未来部署配置或自行准备 TLS 反向代理。

数据库线程独占连接和目录锁；HTTP 通过有界命令队列交互，不直接运行 SQL。队列满和认证请求并发门槛满均返回 503，可重试。已接受命令即使响应接收方取消也排空执行；线程结束后才释放目录锁。进程崩溃依赖 SQLite WAL 恢复和 Interrupted 启动登记；实际资产发布恢复留到 M4。

## 验证证据

新增 14 个测试：1 个执行器单元测试、3 个配置/权限测试、5 个持久化/隔离测试、1 个真实进程信号/重启测试、4 个实际 HTTP 测试。聚焦执行均已 PASS。额外验证包括配置限额下的 chunked 正文、Host 不影响 public URL、关闭数据库后 ready 返回 503 且 health 仍为 200。

文档检查 PASS：33 个 Markdown、137 个本地链接、15 个 JSON 示例、2 个合成 PNG/hash fixture、7 个畸形配对检查。它不执行服务端。

推送前最终代码树的检查均已 PASS：

- `cargo fmt --all --check`
- `cargo clippy --locked --all-targets -- -D warnings`
- `cargo build --locked --all-targets`
- `cargo test --locked --all-targets`（14 个测试）
- `python tools/check-docs.py`
- `git diff --check`

不把设计文档中的 A01–A23 当作已通过：原图/缩略图、同步游标、并发记录条件、资产发布故障和客户端 UI 仍待后续阶段。

远端 CI、真实公网部署、PhotoViewer 接入和同步性能均不属于 M1 的已验证能力。
