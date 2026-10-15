# 多用户：本地实现、管理与隔离验收

日期：2026-10-15。范围为用户名/密码登录、管理员命令行建用户、逐用户照片同步与隔离；线上部署另行记录。接口唯一来源为[认证契约](../contracts/authentication.md)，不开放注册、共享相册、组织权限或跨用户读写。

## 实现边界

`users.rs` 管理私有认证 SQLite：不可变用户 UUID、规范用户名、Argon2id PHC 密码散列、启用标识、授权版本、会话摘要/过期时间。随机盐与 256 位会话秘密取自系统安全随机源；只持久化会话 SHA-256 摘要。改密码/禁用/启用更新授权版本并撤销会话，普通重启保留未过期会话。30 天到期后重新登录，不保存密码做后台自动登录。

`auth.rs` 在处理任何同步操作前按有效会话选择服务端拥有的 `AppState`。每用户有独立 Store、SQLite 写入队列、快照代际、租约与持久 fence、操作回执、对象/暂存目录及签名密钥。用户级密钥从部署根签名密钥以 `syncserver/user-key/v1` 域分离派生。相同字节只在同一账户内去重，不跨用户共享文件或 inode。

异步 OnceCell 只串行化同一用户的首次恢复；打开一个用户的存储不持有所有账户的长锁。失败恢复仅影响该用户，其余用户和全局健康接口可继续工作。当前单进程最多缓存 128 个已打开用户上下文，超限明确返回忙；此容量不表示共享数据或全站同时一个上行锁。密码验证最多并行 2 项，每用户名每分钟最多 8 次登录尝试，尝试表最多 4096 项，过期条目回收。目录和文件仍要求服务 UID、0700/0600、无符号链接等既有约束。

API 使用会话选中的 Extension 上下文；所有兼容别名、指标、资源、租约和幂等状态复用此边界。账户资源必须同时具有该账户会话及有效资源能力；只有 Bearer 或只有签名 URL 均不足以完成账户资源传输。令牌种类、角色、路径、范围、期限、lease/fence 等校验不移除。已开始的请求可能安全结算；撤销后新的请求与续租不得被授权。

PhotoViewer 保存服务器给出的用户/命名空间身份，密码只用于当次请求，系统凭证服务保存会话。相同 endpoint、不同用户可映射不同本地根；同账户/zone 在同一客户端不重复映射，物理根也不能重叠。每次构建任务传输前探测并核对已固定身份，错误账户的凭证不能推进接收或应用位置。任务重新登录只接受原账户；另一账户要建立另一个任务。登录/取消草稿不打断当前任务。

## 管理命令

设置 `SYNC_DATA_DIR`、`SYNC_SIGNING_KEY_FILE`，以服务运行 UID 执行：

```sh
cargo build --locked --bin syncserver
./target/debug/syncserver users create alice
./target/debug/syncserver users create bob
./target/debug/syncserver users list
./target/debug/syncserver users password alice
./target/debug/syncserver users disable alice
./target/debug/syncserver users enable alice
```

创建与改密码在终端关闭回显并确认，Ctrl+C 会取消并恢复终端。自动化使用 `--password-stdin` 从受保护输入读取；不能把密码写到 argv、仓库或日志。CLI 可在服务运行期间管理用户，不直接打开照片 SQLite，也不删除照片。

新部署只需数据目录及私有签名密钥，`SYNC_API_KEY_FILE` 可省略。没有注册用户时尚不能密码登录；先由管理员创建用户，再从客户端设置登录。API key 仅保留给没有用户的旧单账户部署。

## 旧单账户升级

1. 备份原有数据目录和原签名密钥；保留原数据位置、container、zones 和签名配置。
2. 为唯一的旧数据所有者创建账户时显式指定接管：

```sh
./target/debug/syncserver users create owner --adopt-legacy
```

3. 旧照片、generation、token、租约/回执历史仍使用原数据库与签名上下文，不移动文件或重写照片。所有者的新会话与接管前已接受的旧请求共享发布互斥和资源配额。第二个接管用户被拒绝。
4. 创建首个用户后，旧 API key 和无会话签名能力不再授权。在 PhotoViewer 原任务选择「登录并继续」，以新建所有者登录；同一 namespace/generation 的身份升级保留已有同步进度，不重新上传照片。其他新用户使用独立目录，不继承旧数据。

省略 `--adopt-legacy` 的新用户不会得到原数据；原数据也不会删除。已有普通用户可在后续创建唯一的接管者，不会把第一个普通账户自动当成所有者。用户名不作为文件路径，重名用户不会覆盖现有账户。

## 备份和运维边界

备份需包含 `.authentication/users.sqlite`、全部用户目录、已接管的原目录和根签名配置，不能只按用户名猜测恢复路径。认证库与照片库均使用 SQLite 一致备份或停止服务后备份，不复制运行中的裸 WAL。恢复历史认证备份后，管理员应通过禁用/启用撤销恢复出来的旧会话，再让用户重新登录。

本轮不改动阿里云现有部署，不创建真实账户，不读取真实照片做多用户测试；公开服务的当前状态不能用本地测试代替。

## 聚焦验收入口

```sh
cargo test --locked --test users_http --test assets_http --test configuration --test startup_http --test metrics_http -- --test-threads=1
cargo test --locked --lib users::tests -- --test-threads=1
python3 tools/check-user-cli.py --binary target/debug/syncserver
```

`users_http` 用真实 CLI 建用户、真实登录与实际 HTTP/文件路径验证：相同记录/操作/设备 ID 独立；快照与租约不可交叉；相同字节分别落在各账户目录且 inode 不同；资源需双重认证；会话撤销与重启；某账户存储故障不阻塞其他账户；登录限流。CLI 伪终端验证不回显、确认和 Ctrl+C 恢复。

相邻 PhotoViewer 的 `native_sync_accounts` 验证同地址双账户完整照片同步、同用户不同会话接收、错误账户凭证与重新登录、旧单账户接管后保留照片/进度且不重传；`ux_native_sync_settings` 经真实 GTK 控件测试用户名密码登录、保存、注销、重新登录以及既有相册范围流程。客户端仅维护接入说明并链接本仓库协议。

## 本轮实际结果

服务端 **30 个聚焦 Rust 用例通过，0 失败、0 忽略**：`users_http` 7、`assets_http` 6、`configuration` 4、`startup_http` 4、`metrics_http` 5，以及 user/token 指定单元用例 4。伪终端 CLI 检查另行通过，验证不回显、确认和 Ctrl+C 恢复。特定用户目录失败和显式接管者执行器停止时，其他账户仍能读写，账户模式全局就绪不依赖某一用户的执行器。

PhotoViewer **50 个相关用例通过，0 失败、0 忽略**：双账户/旧数据接管实际照片旅程 2、原生资源 9、连接探测 6、完整设置 GTK 控件旅程 1，以及协议与 actor 指定单元用例 32。测试使用临时目录、合成图片、真实新服务端二进制及内存凭证适配，没有访问真实密钥环或照片。旧关系升级保持同一 generation 和进度，照片未修改，上传次数为 0；错误账户会话没有推进原任务进度。

两仓库最终 `cargo fmt --all --check`、各自严格 all-targets Clippy 和实际二进制构建通过。服务端文档检查通过：44 Markdown、254 本地链接、23 JSON 示例和已有合成资源样例。日志在各自忽略的 target 中：服务端 `multiuser-final-{fmt,clippy,build,unit,focused,cli-pty,docs}.log`，客户端 `multiuser-client-final-{fmt,clippy,build,unit,focused}.log`。

**未运行全量测试、远端 CI；按用户指令仅创建本地提交，不推送，未部署阿里云或创建真实用户。** 提交阶段不额外运行测试，以上结果来自开发验证阶段。 本轮结果为本地功能和相关验收，不代替后续线上升级、全量发布闸门及全部故障窗口矩阵。
