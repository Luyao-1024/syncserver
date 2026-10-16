# 运维指标接口

日期：2026-10-15。M5 已实现 `GET /v1/metrics`，capabilities 的 `operationalMetrics=true` 表示支持此版本。此接口是本服务运维扩展，返回 JSON，不属于 CloudKit 同步协议。

## 认证与准入

必须提供当前 namespace 的有效 Bearer API key，重复 Authorization 头拒绝。资源签名 URL、leaseToken 和同步 token 不能授权此接口。响应带 `Cache-Control: no-store` 和 `X-Request-ID`，不得包含凭证、资源 URL、磁盘路径或记录字段。

监控使用独立的单请求配额，不占普通 HTTP、租约控制或资源角色配额；配额耗尽仍返回 `503 SERVER_BUSY`。接口直接采样进程中的计数器和队列状态，不向数据库排队，不查询 SQL，不读取或哈希资产正文。因此普通/控制请求配额耗尽时仍可读取。进程仍服务 HTTP、但数据库已关闭时，持有效 Bearer 的监控请求返回 200 和 `ready=false`；其他同步接口继续返回服务不可用。

`ready` 表示完成启动恢复且数据库执行器仍接收任务，与 `/readyz` 的数据库 ping 不同；监控不证明数据库响应延迟正常。启动恢复失败时进程拒绝启动，没有可抓取的监控端点，失败由退出状态和日志报告。

## 响应与生命周期

顶层字段为 `version`（当前 1）、`runId`、`ready`、`recovery` 和 `metrics`。`runId` 来自本次启动恢复的持久运行标识；每次服务启动改变，不是 database generation。同步 token 的普通重启连续性不变。

`metrics` 和 capabilities 中的同名对象使用同一采样实现。累计计数在本次服务实例内递增，重启从零开始；采集器应按 `runId` 处理计数重置。各字段分别采样，不承诺跨字段原子快照；采样中可能有并发变化。

| metrics 字段 | 语义 |
|---|---|
| requests | 进入 HTTP 中间件的请求数，包含健康、认证失败与本次监控请求 |
| requestRejections | HTTP 准入配额耗尽次数，含普通/控制/监控配额；不包含认证失败、未就绪和领域错误 |
| database.queueDepth / queueCapacity | 普通 SQLite 待处理队列长度/容量，不含当前执行命令 |
| database.queueRejections | 普通 SQLite 队列满而拒绝的命令次数 |
| database.controlQueueDepth / controlQueueCapacity | 独立控制 SQLite 待处理队列长度/容量，不含当前执行命令 |
| database.controlQueueRejections | 控制队列满而拒绝的命令次数，覆盖租约操作及资源 Validate/Fail |
| database.commandsCompleted | SQLite 执行器处理完的普通/控制/维护命令总数，包含错误和响应接收方已取消的命令，不是成功提交数 |
| databaseChangePages | 成功生成的 database 变化响应页数；无变化页也计入，错误不计入 |
| zoneChangePages | 成功生成的 zone 变化页数；多 zone 请求逐 zone 计数，无变化页计入，逐 zone 错误不计入；所有 changes 别名共享计数 |
| assetBytesRead | 下载流实际读取的对象字节数；HEAD 和元数据查询不增加 |
| assetBytesHashed | 上传接收时参与 SHA-256 的字节数；最终校验失败的已处理字节也计入 |
| uploadedBytes | 上传接收器写入并参与摘要的字节数，不表示已 Published 或已绑定照片 |
| httpAdmission.capacity / active | 普通 HTTP 配额/占用数；占用持续至响应头和响应体流建立，不表示下载流整个生命周期 |
| httpAdmission.controlCapacity / controlActive | 租约控制 HTTP 配额/占用数；不包含监控请求 |
| transfers.originalUploads / thumbnailUploads | 各角色当前持有上传配额的任务数 |
| transfers.originalDownloads / thumbnailDownloads | 各角色当前持有下载配额的任务数；下载响应体结束/取消释放配额 |
| logging.persistLevel / activated | 落库等级；activated 表示日志库已随数据目录锁打开，未激活时其余日志字段缺省 |
| logging.queueDepth / queueCapacity | 待落库队列长度/容量，不含写入线程正在批处理或已提交的内容 |
| logging.persisted | 已提交进日志库的记录数，含未压缩窗口与已压缩段，不区分二者 |
| logging.droppedQueueFull | 队列满时丢弃的最新记录数，丢弃不阻塞请求线程 |
| logging.droppedPending | 激活前内存缓冲溢出时丢弃的最旧记录数，例如启动读取凭证阶段的日志 |
| logging.writeFailures | 日志库提交或归档失败次数，失败只影响日志本身 |
| logging.rawRows / oldestRawTsMs | 未压缩窗口行数与最早时间戳，窗口为空时时间戳为 0；当天窗口不设上限，因此行数可持续增长 |
| logging.archivedRows / archiveSegments / archiveBytes / archiveMaxBytes | 已压缩段行数/段数/字节数与生效上限 |
| logging.trimmedSegments | 因超过归档上限而整段删除的最旧段数 |
| logging.lastArchiveTsMs | 最近一次归档 pass 的时刻，包括未产生新段的检查 |

`metrics.logging` 全部来自进程内增量计数器，读取监控不打开、不查询、也不锁 `logs.sqlite`，因此日志库损坏或归档进行中都不会阻塞此接口；日志留存阈值与压缩语义见[日志与留存契约](logging.md)。当 `SYNC_LOG_PERSIST_LEVEL=off` 时该对象只有 `enabled=false`、`persistLevel` 和 `activated=false`。

`recovery` 与 capabilities 中同名对象一致，包含 runId、interruptedStarts、assetBytesRead、assetBytesHashed；它记录本次启动恢复的固定结果，恢复读取/哈希成本不混入运行期传输计数。队列关闭后深度为零，不能从深度推导历史处理成功。

所有页数只表示服务端完成生成响应，不证明客户端收到、持久化或应用了变化。任何监控计数都不能代替 revision、digest、同步 token、上行 check 或客户端 appliedToken。

## 验证入口

`tests/metrics_http.rs` 验证真实 HTTP 认证、零数据库工作采样、普通/控制配额饱和、别名/分页/逐 zone 错误、进程重启计数以及真实部署 smoke；`tests/startup_http.rs` 验证执行器关闭后的诊断读取。`tests/assets_http.rs` 用真实双资源上传/下载和 100 次空变化/HEAD 验证成本计数。数据库单元测试阻塞实际执行线程，分别填满两个队列验证容量、深度、拒绝计数与关闭排空。

`tests/logging.rs` 断言这些日志字段的底层计数：归档超限裁剪后 `archiveBytes`/`trimmedSegments`/`rawRows` 的实际取值，以及队列溢出时 `droppedQueueFull`/`droppedPending`/`persisted` 的有界行为；`tests/logging_process.rs` 用真实进程验证 dev/prod 两档的等级与 detail 差异。未激活或未启用落库时接口返回 `enabled=false`，`tests/metrics_http.rs` 验证采样不产生数据库工作。
