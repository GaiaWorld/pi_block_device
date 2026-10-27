//! Internal superblock state representation.

use rpds::RedBlackTreeMapSync;

use bytes::Bytes;
use pi_block_device::{BlockRef, RecordSeq};

use crate::{ApplicationCodec, ApplicationState, BindingName, DeviceTable, SuperblockError};
#[cfg(test)]
mod atomic_tests;

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SuperblockStatus {
    Ready,
    ReadOnlyExhausted,
    RecoveryRequired,
}

/// Monotonic runtime status; recovery can only be cleared by constructing a reopened owner.
pub(crate) struct AtomicStatus(std::sync::atomic::AtomicU8);

impl AtomicStatus {
    pub(crate) fn new(status: SuperblockStatus) -> Self {
        Self(std::sync::atomic::AtomicU8::new(Self::encode(status)))
    }

    fn encode(status: SuperblockStatus) -> u8 {
        match status {
            SuperblockStatus::Ready => 0,
            SuperblockStatus::ReadOnlyExhausted => 1,
            SuperblockStatus::RecoveryRequired => 2,
        }
    }

    pub(crate) fn load(&self) -> SuperblockStatus {
        match self.0.load(std::sync::atomic::Ordering::Acquire) {
            0 => SuperblockStatus::Ready,
            1 => SuperblockStatus::ReadOnlyExhausted,
            _ => SuperblockStatus::RecoveryRequired,
        }
    }

    pub(crate) fn update(&self, status: SuperblockStatus) {
        self.0
            .fetch_max(Self::encode(status), std::sync::atomic::Ordering::AcqRel);
    }
}

#[doc(hidden)]
pub struct RecoveredState<S: ApplicationState> {
    pub state: SuperblockState<S>,
    pub status: SuperblockStatus,
}

#[derive(Clone)]
pub struct SuperblockState<S: ApplicationState> {
    pub record_seq: RecordSeq,
    pub devices: DeviceTable,
    pub roots: RedBlackTreeMapSync<BlockRef, RecordSeq>,
    pub bindings: RedBlackTreeMapSync<BindingName, BlockRef>,
    pub application: S,
}

#[doc(hidden)]
pub fn decode_checkpoint_state<S, C>(
    record_seq: RecordSeq,
    bytes: Bytes,
    codec: &C,
) -> Result<SuperblockState<S>, SuperblockError>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
{
    let (devices, roots, bindings, application) =
        crate::decode_checkpoint(record_seq, bytes, codec)?;
    Ok(SuperblockState {
        record_seq,
        devices,
        roots,
        bindings,
        application,
    })
}

pub(crate) fn allocations_are_not_in_the_future(
    state: &pi_block_device::BlockDeviceState,
    current: RecordSeq,
) -> bool {
    state.levels.iter().all(|level| {
        level.versions.iter().flatten().all(|segment| {
            segment
                .allocation_runs
                .iter()
                .all(|run| run.allocated_at <= current)
        })
    }) && state
        .large_blocks
        .entries
        .values()
        .all(|data| data.allocated_at <= current)
}

pub(crate) fn is_live_block(devices: &DeviceTable, block: BlockRef, current: RecordSeq) -> bool {
    let Some(slot) = devices.get(block.device) else {
        return false;
    };
    let state = slot.state();
    if let Some(id) = block.block.large_id() {
        return state
            .large_blocks
            .entries
            .get(&id)
            .is_some_and(|data| data.allocated_at <= current && data.released_at.is_null());
    }
    let Some((version, index)) = block.block.ordinary_fields() else {
        return false;
    };
    let Some(level) = state.levels.get(block.block.level() as usize) else {
        return false;
    };
    let Some(offset) = version.raw().checked_sub(level.min_version.raw()) else {
        return false;
    };
    let Some(Some(segment)) = level.versions.get(offset as usize) else {
        return false;
    };
    let Some(released_at) = segment.released_at.get(index as usize) else {
        return false;
    };
    let mut slot_start = 0_u64;
    for run in &segment.allocation_runs {
        let Some(slot_end) = slot_start.checked_add(u64::from(run.len)) else {
            return false;
        };
        if u64::from(index) < slot_end {
            return u64::from(index) >= slot_start
                && run.allocated_at <= current
                && released_at.is_null();
        }
        slot_start = slot_end;
    }
    false
}

const _: fn() = || {
    fn send_sync<T: Send + Sync>() {}
    fn state<S: ApplicationState>() {
        send_sync::<SuperblockState<S>>();
        send_sync::<std::sync::Arc<SuperblockState<S>>>();
    }
    let _ = state::<crate::state::SendSyncApplication>;
};

#[derive(Clone)]
struct SendSyncApplication;
impl ApplicationState for SendSyncApplication {
    type Delta = ();
}
