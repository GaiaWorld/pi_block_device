//! 分配、释放和快照状态。
//! @api @impl @design-doc: design/features/13-wire-format.feature

use crate::{LargeBlockCursor, LargeBlockId, RecordSeq, SegmentVersion};
use rpds::{RedBlackTreeMapSync, VectorSync};
use std::num::NonZeroU32;

/// A contiguous range of ordinary block slots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockRange {
    pub start: u32,
    pub len: NonZeroU32,
}

/// 一次连续分配运行。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllocationRun {
    /// 分配发生的记录序号。
    pub allocated_at: RecordSeq,
    /// 连续槽位数。
    pub len: u32,
}
/// 一个普通段版本的分配和释放数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegmentData {
    /// 分配运行列表。
    pub allocation_runs: VectorSync<AllocationRun>,
    /// 每个槽位对应的释放序号。
    pub released_at: VectorSync<RecordSeq>,
}
impl SegmentData {
    /// 校验结构、分配顺序和释放序号的严格生命周期关系。
    ///
    /// 此方法不校验释放序号相对于当前记录的上界；使用 validate_at 执行带当前记录上界的校验。
    pub fn validate(&self) -> bool {
        let mut previous = RecordSeq::NULL;
        let mut total = 0_u64;
        for run in self.allocation_runs.iter() {
            if run.len == 0 || run.allocated_at <= previous {
                return false;
            }
            total += u64::from(run.len);
            // 槽位索引包含 u32::MAX，故总数量可到 2^32；不能以 u32 累加。
            if total > u64::from(u32::MAX) + 1 {
                return false;
            }
            previous = run.allocated_at;
        }
        if total != self.released_at.len() as u64 {
            return false;
        }
        // runs 隐式连续且不重叠，按同一顺序逐槽匹配所属分配记录。
        let mut releases = self.released_at.iter();
        for run in self.allocation_runs.iter() {
            if !releases
                .by_ref()
                .take(run.len as usize)
                .all(|release| release.is_null() || *release > run.allocated_at)
            {
                return false;
            }
        }
        true
    }

    /// 在指定当前记录下校验释放序号的上界。
    /// 前置条件：current_record 是有效持久记录，非生命周期 Null。
    pub fn validate_at(&self, current_record: RecordSeq) -> bool {
        self.validate()
            && self
                .released_at
                .iter()
                .filter(|released_at| !released_at.is_null())
                .all(|released_at| *released_at <= current_record)
    }
}
/// 一个块级别的段版本窗口。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LevelVersions {
    /// 窗口起始版本。
    pub min_version: SegmentVersion,
    /// 按版本排列的段数据，尾部必须是活动段。
    pub versions: VectorSync<Option<SegmentData>>,
}
impl LevelVersions {
    /// 以给定版本构造唯一 Some 尾项，两组空 Vector 表示虚拟空 Active。
    pub fn initial(version: SegmentVersion) -> Self {
        Self {
            min_version: version,
            versions: rpds::vector_sync![Some(SegmentData {
                allocation_runs: VectorSync::new_sync(),
                released_at: VectorSync::new_sync(),
            })],
        }
    }
    /// 校验窗口结构和段数据有效性，不检查释放序号的当前记录上界。
    pub fn validate(&self) -> bool {
        let len = self.versions.len();
        if len == 0 || !matches!(self.versions.last(), Some(Some(_))) {
            return false;
        }
        // versions[i] 对应 min_version+i，尾版本不得超出冻结的 28 位范围。
        if len as u64 > u64::from(0x0fff_ffff - self.min_version.raw()) + 1 {
            return false;
        }
        self.versions.iter().enumerate().all(|(index, entry)| {
            entry.as_ref().is_none_or(|segment| {
                segment.validate() && (index == len - 1 || !segment.allocation_runs.is_empty())
            })
        })
    }

    /// 在指定当前记录下校验窗口中的所有释放序号。
    /// 前置条件：current_record 是有效持久记录，非生命周期 Null。
    pub fn validate_at(&self, current_record: RecordSeq) -> bool {
        self.validate()
            && self
                .versions
                .iter()
                .filter_map(Option::as_ref)
                .all(|segment| segment.validate_at(current_record))
    }
}
/// 一个独立大块的生命周期数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LargeBlockData {
    /// 分配发生的记录序号。
    pub allocated_at: RecordSeq,
    /// 释放记录序号，Null 表示仍然活动。
    pub released_at: RecordSeq,
    /// 逻辑长度。
    pub logical_len: u64,
}
/// 独立大块索引及其分配游标。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LargeBlocks {
    /// 下一个大块游标。
    pub next_id: LargeBlockCursor,
    /// 大块生命周期索引。
    pub entries: RedBlackTreeMapSync<LargeBlockId, LargeBlockData>,
}
impl LargeBlockData {
    /// 校验大块生命周期字段，不检查释放序号的当前记录上界。
    pub fn validate(&self) -> bool {
        !self.allocated_at.is_null()
            && self.logical_len > 0
            && (self.released_at.is_null() || self.released_at > self.allocated_at)
    }

    /// 在指定当前记录下校验释放序号的上界。
    /// 前置条件：current_record 是有效持久记录，非生命周期 Null。
    pub fn validate_at(&self, current_record: RecordSeq) -> bool {
        self.validate() && (self.released_at.is_null() || self.released_at <= current_record)
    }
}
impl LargeBlocks {
    /// 校验大块游标边界及生命周期数据，不检查释放序号的当前记录上界。
    pub fn validate(&self) -> bool {
        self.entries
            .iter()
            .all(|(id, data)| id.raw() < self.next_id.raw() && data.validate())
    }

    /// 在指定当前记录下校验所有大块释放序号的上界。
    /// 前置条件：current_record 是有效持久记录，非生命周期 Null。
    pub fn validate_at(&self, current_record: RecordSeq) -> bool {
        self.validate()
            && self
                .entries
                .values()
                .all(|data| data.validate_at(current_record))
    }
}
/// 已提交的完整设备分配状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockDeviceState {
    /// 十五个固定级别的版本窗口。
    pub levels: [LevelVersions; 15],
    /// 独立大块状态。
    pub large_blocks: LargeBlocks,
}
impl BlockDeviceState {
    /// 十五级均从 version=1 的虚拟空 Active 开始，大块游标=1、索引为空。
    /// 保留 Result 签名以沿用 SegmentVersion/LargeBlockCursor 的 checked 构造；
    /// 固定初始值合法，不引入新的失败协议。
    pub fn initial() -> Result<Self, crate::InvalidBlockId> {
        let version = SegmentVersion::new(1)?;
        Ok(Self {
            levels: std::array::from_fn(|_| LevelVersions::initial(version)),
            large_blocks: LargeBlocks {
                next_id: LargeBlockCursor::new(1)?,
                entries: RedBlackTreeMapSync::new_sync(),
            },
        })
    }
    /// 校验十五级窗口及大块生命周期字段，不检查释放序号的当前记录上界。
    pub fn validate(&self) -> bool {
        self.levels.iter().all(LevelVersions::validate) && self.large_blocks.validate()
    }

    /// 在指定当前记录下校验所有释放序号的上界。
    /// 前置条件：current_record 是有效持久记录，非生命周期 Null。
    pub fn validate_at(&self, current_record: RecordSeq) -> bool {
        self.levels
            .iter()
            .all(|level| level.validate_at(current_record))
            && self.large_blocks.validate_at(current_record)
    }
}

const _: fn() = || {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<BlockDeviceState>();
};
