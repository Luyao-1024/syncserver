# 客户端配对发布与索引屏障 v1

状态：设计选择已固定；仅 C1 在 PhotoViewer 实现，当前不修改该仓库代码。本服务端实现不假装已验证客户端。

## 持久台账与读取边界

C1 增加由 SyncStore/DbActor 管理的本地 sync_publications 台账：operationId、jobId、photoId、contentGeneration、targetPath、stageOriginal/stageThumbnail、oldVersion/backupPath、cachePlan、state、configGeneration、失败原因。

目标路径在文件发布前持久登记为 blocked。scanner、watcher 和同步直接入库都必须经同一个索引准入检查；blocked 路径的观察只排队，不创建/刷新 media_items。不使用 UI 可见性设置隐藏半成品。

新照片未 Ready 时没有 media_items 行。已存在照片保留旧行。更新的原图读入口采用本地 publication read lease：Publishing 状态解析到旧备份，Ready 后解析到新原图，保证本应用不读取尚未结算的新版本。缩略图读取使用相同旧版本 stamp。lease 是本地文件读取适配，不涉及远端占位或网络懒下载。

lease 原子选择版本并打开只读文件句柄，同时提供源 mtime/缓存 stamp；不能只返回路径后延迟打开，否则 rename 仍可导致 TOCTOU。当前解码入口的文件打开需薄适配到该句柄/stream，缓存键使用相同 stamp，不能绕过屏障直接 stat 新目标。

lease 接入原图解码、缩略图源解析和编辑读取边界；编辑/删除等写操作与发布按目标路径互斥。备份只有最后一个旧版本读取 lease 结束后才清理。外部程序不受本应用 lease 保护，不能宣称跨进程可见性事务。

## 固定顺序

1. 双资源 staging 校验并持久保存 VerifiedPair，核对最新 job/config/source revision。
2. 登记 blocked 与 Prepared；保留旧行和旧读取版本。更新时先生成稳定旧快照（reflink/复制并校验、fsync；不使用共享可写 inode 的硬链接），持久登记后将 lease 路由切到旧快照，再进入 Publishing。已打开旧文件句柄保持可用，不能在旧快照不存在时让读取路由指向它。
3. 后台准备新缓存，按最终路径和原图 mtime 映射尺寸；旧版本缓存继续可用。
4. 通过可恢复发布意图结算原图与新缓存；失败恢复旧完整版本。
5. 同一客户端数据库事务执行媒体 upsert、成功基线、LocalReady 和解除 blocked；提交后发送版本事件，重放 watcher 积压。
6. 本地显示 token/version stamp 随事件失效；晚完成旧读取不能覆盖新版本。旧备份/缓存按 lease 安全清理。

只有步骤 5 才是本应用的新可见版本提交点。不存在“先解除屏障、之后再写成功基线”的路径。

## 恢复

启动先读取台账并恢复 Publishing，再开始图库查询与 scanner/watcher。可证明完整发布的操作完成第 5 步；其他恢复旧原图/缓存/行并保留待办。不仅凭目标文件存在标记成功。

新照片失败时清理/隔离未提交最终文件，不入库；旧照片失败时恢复旧版本。下载成功不是发布成功，cache import 失败也不能绕过屏障。

## 缓存键冲突

现有 path+mtime 键可继续使用，但若不同内容生成同键，导入器不能原地覆盖仍被旧 lease 使用的缓存。该缓存版本在同步台账的 cachePlan 中暂存并保持版本区分；Ready 事务后精准失效/切换相关内存键，不能清空全库缓存或将新缓存泄漏给旧请求。

可通过局部版本映射与源解析薄适配实现，具体 Rust helper 在 C1 决定；必须保留上述提交点和 lease 语义，不再开放“是否需要屏障”的选择。

## 验收

两个下载完成之前不可见；watcher 抢先事件；cache import/原图发布/DB 提交各阶段失败；更新期间原图读取及编辑竞争；同键不同 generation；旧解码晚回调；进程重启；旧文件仅在 lease 结束后清理。全部使用实际文件/事务与真实 UI 输入。

关联：[完整媒体同步](media-sync.md)、[恢复](concurrency-recovery.md)、[验收](verification.md)。
