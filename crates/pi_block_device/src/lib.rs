//! 固定块设备协议的基础值对象。

mod allocation;
mod backend;
mod error;
mod geometry;
mod id;

pub use allocation::{
    AllocationRun, BlockDeviceState, BlockRange, LargeBlockData, LargeBlocks, LevelVersions,
    SegmentData,
};
pub use backend::{
    BlockDeviceBackend, BlockDeviceWriteBatch, BoxError, DeviceError, DeviceOperation,
    InspectReport, ProtectedReadBuf, ReconcileError, ReconcileFailureKind, ReconcileProgress,
    ReconcileReport, RetiredObject, RetirementReport,
};
pub use error::{
    InvalidBlockId, InvalidBlockWriteChunk, InvalidBlockWritePosition, RecordSeqExhausted,
};
pub use geometry::{BackendCapabilities, BlockWriteChunk, BlockWritePosition};
pub use id::{
    BlockDeviceId, BlockId, BlockRef, DeviceKey, LargeBlockCursor, LargeBlockId, RecordSeq,
    SegmentVersion,
};

/// 标识协议值对象的公开模块。
pub mod protocol {
    /// 标识协议值对象。
    pub mod id {
        pub use crate::{
            BlockDeviceId, BlockId, DeviceKey, LargeBlockCursor, LargeBlockId, RecordSeq,
            RecordSeqExhausted, SegmentVersion,
        };
    }
}

/// 分配状态类型的公开模块。
pub mod allocation_state {
    pub use crate::{
        AllocationRun, BlockDeviceState, BlockRange, LargeBlockData, LargeBlocks, LevelVersions,
        SegmentData,
    };
}
