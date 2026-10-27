//! 块设备后端与写入批次的协议接口。

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::sync::Arc;

use crate::{
    BackendCapabilities, BlockDeviceId, BlockDeviceState, BlockId, BlockWriteChunk, DeviceKey,
    LargeBlockId, SegmentVersion,
};

/// 可在线程间传递的后端错误来源。
pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// 标识一次设备操作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceOperation {
    /// 检查介质。
    Inspect,
    /// 开始写入批次。
    BeginWriteBatch,
    /// 写入数据。
    Write,
    /// 准备持久布局。
    Prepare,
    /// 获取快照租约。
    AcquireSnapshotLease,
    /// 读取块。
    Read,
    /// 加入退休队列。
    EnqueueRetirements,
    /// 执行一轮退休回收。
    ReclaimRetirements,
}

impl fmt::Display for DeviceOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Inspect => "inspect",
            Self::BeginWriteBatch => "begin_write_batch",
            Self::Write => "write",
            Self::Prepare => "prepare",
            Self::AcquireSnapshotLease => "acquire_snapshot_lease",
            Self::Read => "read",
            Self::EnqueueRetirements => "enqueue_retirements",
            Self::ReclaimRetirements => "reclaim_retirements",
        };
        formatter.write_str(name)
    }
}

/// 设备操作失败。
#[derive(Debug)]
pub struct DeviceError {
    /// 失败发生的操作。
    pub operation: DeviceOperation,
    /// 可选的设备注册键。
    pub device_key: Option<DeviceKey>,
    /// 底层错误来源。
    pub source: BoxError,
}

impl fmt::Display for DeviceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.device_key {
            Some(key) => write!(
                formatter,
                "device operation {} failed for device {}: {}",
                self.operation,
                key.raw(),
                self.source
            ),
            None => write!(
                formatter,
                "device operation {} failed: {}",
                self.operation, self.source
            ),
        }
    }
}
impl Error for DeviceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// 介质检查结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InspectReport {
    /// 介质是否没有可识别对象。
    pub empty: bool,
    /// 观察到的对象数量。
    pub observed_objects: u64,
}

/// 成功完成一整轮退休回收的统计，不是队列余额或物理删除数。
/// `attempted = completed + guarded + retryable`；Err 或取消没有整轮统计可依赖。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetirementReport {
    /// 本轮选定并尝试处理的对象数，每个对象最多尝试一次。
    pub attempted: u64,
    /// 已确认责任完成的对象数，包含已确认不存在的对象。
    pub completed: u64,
    /// 因现有 lease 保护而保留责任的对象数。
    pub guarded: u64,
    /// 因单对象删除失败或证据未决而保留责任的对象数。
    pub retryable: u64,
}

/// 对账失败的类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileFailureKind {
    /// 必需对象缺失。
    RequiredObjectMissing,
    /// 必需对象损坏。
    RequiredObjectCorrupt,
    /// 修复写入失败。
    RepairIo,
    /// 介质元数据同步失败。
    MediaMetadataSync,
    /// 检测到并发变更。
    ConcurrentMutation,
}
impl fmt::Display for ReconcileFailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self)
    }
}

/// 对账过程进度。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconcileProgress {
    /// 计划执行的动作数。
    pub planned_actions: u64,
    /// 已完成的动作数。
    pub completed_actions: u64,
    /// 尚未完成的动作数。
    pub remaining_actions: u64,
    /// 是否产生部分进度。
    pub partial_progress: bool,
    /// 当前故障模型所需 namespace 操作和文件同步均已完成；不表示父目录 fsync。
    /// 排除突然掉电导致新文件名、rename/remove 目录项未落盘。
    pub media_metadata_synced: bool,
}

/// 对账成功报告。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconcileReport {
    /// 检查过的对象数量。
    pub inspected_objects: u64,
    /// 对账进度。
    pub progress: ReconcileProgress,
}

/// 对账失败报告。
#[derive(Debug)]
pub struct ReconcileError {
    /// 失败类别。
    pub kind: ReconcileFailureKind,
    /// 失败时的进度。
    pub progress: ReconcileProgress,
    /// 可选的底层错误来源。
    pub source: Option<BoxError>,
}
impl fmt::Display for ReconcileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "reconcile failed kind={} completed={} planned={}",
            self.kind, self.progress.completed_actions, self.progress.planned_actions
        )?;
        if let Some(source) = &self.source {
            write!(formatter, ": {source}")
        } else {
            Ok(())
        }
    }
}
impl Error for ReconcileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn Error + 'static))
    }
}

/// 待退休的介质对象。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetiredObject {
    /// 普通段对象，level 为 0..=14；不包含 level 15 独立大块。
    Segment { level: u8, version: SegmentVersion },
    /// 独立大块对象。
    LargeBlock { id: LargeBlockId },
}

/// 受快照租约保护的读取缓冲区。
pub trait ProtectedReadBuf: AsRef<[u8]> + Send + 'static {
    /// 与缓冲区生命周期绑定的快照租约。
    type Lease: Send + Sync + 'static;
    /// 返回缓冲区持有的租约。
    fn lease(&self) -> &Self::Lease;
}

/// 不承载事务身份的块写入批次。
pub trait BlockDeviceWriteBatch: Send + 'static {
    /// 写入一个已经校验的分片。
    fn write(
        &mut self,
        request: BlockWriteChunk,
    ) -> impl Future<Output = Result<(), DeviceError>> + Send + '_;
    /// 消费批次；成功表示数据完整、最终布局可由恢复状态定位，
    /// 所有必要文件 DataAndMetadata 同步真实成功先于 metadata durable。
    /// 2026-10-08 用户收窄故障模型：排除突然掉电导致新文件名、rename/remove
    /// 目录项未落盘；操作错误、原子非覆盖发布、取消和进程退出恢复仍须保证。
    /// Drop 仅释放资源，不删除可能已提交的数据；未提交残留由
    /// 基于权威恢复状态的 reconcile 回收，Unknown 由 pi_superblock 恢复裁决。
    fn prepare(self) -> impl Future<Output = Result<(), DeviceError>> + Send
    where
        Self: Sized;
}

/// 提供检查、写入、快照读取和对账能力的块设备后端。
pub trait BlockDeviceBackend: Send + Sync + 'static {
    /// 快照读取租约类型。
    type SnapshotLease: Send + Sync + 'static;
    /// 读取结果缓冲类型。
    type ReadBuf: ProtectedReadBuf<Lease = Self::SnapshotLease>;
    /// 写入批次类型。
    type WriteBatch: BlockDeviceWriteBatch;
    /// 返回稳定设备身份。
    fn device_id(&self) -> BlockDeviceId;
    /// 返回固定协议能力。
    fn capabilities(&self) -> BackendCapabilities;
    /// 检查介质对象。
    fn inspect(&self) -> impl Future<Output = Result<InspectReport, DeviceError>> + Send + '_;
    /// 同步创建空写入批次，不执行 I/O 或等待写锁，也不承载事务身份。
    fn begin_write_batch(self: &Arc<Self>) -> Result<Self::WriteBatch, DeviceError>;
    /// 基于已提交状态获取快照租约。
    fn acquire_snapshot_lease(
        self: &Arc<Self>,
        state: Arc<BlockDeviceState>,
    ) -> Result<Self::SnapshotLease, DeviceError>;
    /// 从快照租约读取指定块。
    /// 上层保证 backend 与 lease 正确配对，且已在固定 State 中验证 block live；
    /// 不要求后端重复 live 校验或引入新的 owner token。
    fn read<'a>(
        self: &'a Arc<Self>,
        lease: &'a Self::SnapshotLease,
        block: BlockId,
    ) -> impl Future<Output = Result<Self::ReadBuf, DeviceError>> + Send + 'a;
    /// open 恢复阶段将介质对账到权威期望状态，不是在线维护入口。
    fn reconcile(
        self: &Arc<Self>,
        expected: Arc<BlockDeviceState>,
    ) -> impl Future<Output = Result<ReconcileReport, ReconcileError>> + Send + '_;
    /// 仅登记并去重退休责任，不启动任务；由外部定时 await 回收。
    /// 内存队列不是权威持久状态，登记和回收不改变 durable 事务终态。
    /// Err 不证明所有对象均未登记；上层进入恢复围栏，重新 open 从权威状态推导责任。
    fn enqueue_retirements(
        self: &Arc<Self>,
        objects: Vec<RetiredObject>,
    ) -> Result<(), DeviceError>;
    /// 执行一次有限回收轮；内部异步串行化重叠调用，不暴露 token。
    /// 取得执行资格时确定本轮对象范围，之后新入队对象留给下一轮；
    /// 仅使用队列长度 budget 不足以保证对象范围。取得资格后若为空，立即返回全零报告。
    /// 每个选定对象最多尝试一次；Guarded 或单对象删除失败保留责任并继续后续对象，
    /// 后者计 retryable。无 sleep、timer、spawn 或空队列等待。
    /// 只有无法安全继续的内部错误才返回 DeviceError；此前完成不撤销，
    /// Err 没有整轮统计可依赖。取消保留未确认责任，底层已提交删除可以继续，
    /// 下轮按证据确认；Unknown 不得盲目完成或盲删。
    /// lease 注册与删除必须并发安全：已有 lease 保护的对象不得删除，
    /// 删除已开始后不得授予对应对象不安全的新 lease；此合同不指定锁算法。
    /// 不改变 durable 事务终态，内存队列不取代权威持久状态。
    fn reclaim_retirements(
        self: &Arc<Self>,
    ) -> impl Future<Output = Result<RetirementReport, DeviceError>> + Send + '_;
}
