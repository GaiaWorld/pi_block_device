use pi_block_device::{BlockRange, BlockRef, DeviceKey, LargeBlockId, SegmentVersion};

use crate::BindingName;

mod implementation;

#[doc(hidden)]
#[derive(Debug, Eq, PartialEq)]
pub enum SystemChange {
    CreateActiveSegment {
        device: DeviceKey,
        level: u8,
        version: SegmentVersion,
    },
    AllocateSegmentBlocks {
        device: DeviceKey,
        level: u8,
        version: SegmentVersion,
        len: u32,
    },
    ReleaseSegmentBlocks {
        device: DeviceKey,
        level: u8,
        version: SegmentVersion,
        ranges: Vec<BlockRange>,
    },
    RemoveSegment {
        device: DeviceKey,
        level: u8,
        version: SegmentVersion,
    },
    AllocateLargeBlock {
        device: DeviceKey,
        id: LargeBlockId,
        logical_len: u64,
    },
    ReleaseLargeBlock {
        device: DeviceKey,
        id: LargeBlockId,
    },
    RemoveLargeBlock {
        device: DeviceKey,
        id: LargeBlockId,
    },
    AddRoot {
        block: BlockRef,
    },
    RemoveRoot {
        block: BlockRef,
    },
    AddBinding {
        name: BindingName,
        block: BlockRef,
    },
    RemoveBinding {
        name: BindingName,
    },
}

#[doc(hidden)]
#[derive(Debug, Eq, PartialEq)]
pub enum TransactionChange<D> {
    System(SystemChange),
    Application(D),
}

pub(crate) fn apply_system_change<S>(
    state: &mut crate::SuperblockState<S>,
    change: &SystemChange,
    record_seq: pi_block_device::RecordSeq,
) -> Result<(), crate::SuperblockError>
where
    S: crate::ApplicationState,
{
    implementation::apply_system_change(state, change, record_seq)
}
