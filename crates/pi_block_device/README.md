# pi_block_device

当前提供固定物理块设备协议和值对象：BlockDeviceId、DeviceKey、BlockId/BlockRef、SegmentVersion、RecordSeq、固定十五级块几何、分配状态，以及带 Send future 的 backend/write-batch、快照租约、读取、reconcile 和 retirement 接口。TransactionId 定义于 pi_superblock，不属于本 crate。依赖方向为 pi_superblock -> pi_block_device，本 crate 不依赖 pi_superblock。

写入协议：`BlockDeviceBackend::WriteBatch` 实现 `BlockDeviceWriteBatch`，仅提供 `write(&mut self, BlockWriteChunk)` 和消费自身的 `prepare(self)`，两者返回 `Send` future 的 `Result<(), DeviceError>`。`begin_write_batch(self: &Arc<Self>) -> Result<Self::WriteBatch, DeviceError>` 同步创建空批次，不执行 I/O 或等待写锁，也不接收事务身份。无旧终态、多段或 stream 后端 API，也无 PreparedBatch token。

2026-10-08 用户收窄原合同故障模型：排除突然掉电导致新文件名、rename/remove 目录项未落盘。prepare 成功仍要求数据完整、最终布局可恢复定位，必要文件 DataAndMetadata 同步真实成功先于 metadata durable；创建发布删除错误、原子 noreplace、取消及进程退出恢复仍考虑。media_metadata_synced 表示当前模型所需操作/文件 sync 完成，不表示父目录 fsync。Drop 仅释放资源，不删除可能已提交的数据；残留由权威 State 驱动 reconcile，Unknown 由 pi_superblock 恢复裁决。文件后端切片已实现，不代表 Superblock 端到端排序通过。

该 crate 只定义协议、状态和错误边界，不包含文件布局、具体 I/O、superblock、application state 或 append-log 实现。

## 退休回收声明

已批准 `reclaim_retirements(self: &Arc<Self>) -> impl Future<Output = Result<RetirementReport, DeviceError>> + Send + '_` 与 `DeviceOperation::ReclaimRetirements`（display 为 `reclaim_retirements`）。`RetirementReport` 从根导出，derive 与 InspectReport 相同；attempted/completed/guarded/retryable 均为 u64。仅成功整轮报告满足 `attempted = completed + guarded + retryable`，completed 包含已确认不存在，不是物理删除数；报告不是队列余额。

enqueue 仅登记去重，不启动任务；外部定时 await 回收。每次调用是一轮有限工作，重叠调用内部异步串行化，不暴露 token。取得执行资格时确定本轮对象范围，后续新入队留给下一轮；单纯长度 budget 不保证对象范围。取得资格后空轮立即全零返回，每个选定对象最多尝试一次。Guarded 和单对象删除失败保留责任且继续后续，单对象失败计 retryable；不 sleep/timer/spawn，不等待空队列。

只有无法安全继续的内部错误才返回 DeviceError；Err 前完成不撤销，且无整轮统计可依赖。取消保留未确认责任，底层已提交删除可继续，下轮按证据确认；Unknown 不盲完成或盲删。lease 注册与删除必须并发安全，已有 lease 保护的对象不得删除，删除已开始后不得授予对应对象不安全的新 lease；不在此决定锁算法。不改变 durable 事务终态，内存队列不是权威持久状态。文件后端已有有限异步回收，生产线程已移除；Superblock 责任登记/恢复尚未端到端接入。

## 持久化状态

精确依赖 `rpds = "=1.1.0"`，已核实源码：`RedBlackTreeMapSync<K,V>` 和 `VectorSync<T>` 使用 `archery::ArcTK`（`triomphe::Arc`，不是默认 Rc）。`LargeBlocks.entries` 为有序树；`LevelVersions.versions`、`SegmentData.allocation_runs/released_at` 为 Vector。`BlockDeviceState.levels` 仍是固定 `[LevelVersions; 15]`。

集合 clone 结构共享；这不取代 backend lease、并发保护或回收责任。有序编码、版本窗口和生命周期合同不变，wire 格式不升级。`BlockDeviceState::initial`、`LevelVersions::initial/validate` 和 `SegmentData::validate` 已实现，`BlockDeviceState::validate/validate_at` 不再传递进入 todo。初始状态为十五级 version=1 的虚拟空 Active、大块游标=1、空索引。窗口非空、版本连续且不超出合法 SegmentVersion 范围，并无最多两项的合同；尾项为 Some Active，非尾部 Some 不得为空。Vector 前部裁剪属于上层状态变更，本轮未实现。

`validate_at` 的 current_record 前置条件是有效持久记录，Null 仅用于生命周期字段；方法只追加释放上界校验，不新增 allocation 上界或非法 current 防御。backend/lease 正确配对与固定 State 的 live 验证由上层负责，不要求 backend 重复 live 校验或新 owner token。reconcile 仅用于 open 恢复，不是在线维护；enqueue Err 不证明全未登记，上层进入恢复并重新推导责任。普通 RetiredObject::Segment 的 level 为 0..=14。

本 crate 的旧 session/TransactionId 测试已迁移到 WriteBatch 和 rpds Sync 集合，原状态断言保留。新增状态边界测试和完整 backend trait mock；mock 仅验证签名、Send future、报告关系与模型 lease 生命周期，不验证生产回收引擎。文件后端已有公共真实 I/O 测试迁移，superblock 旧测试未迁移，全 workspace 历史迁移仍未验收。
