//! Static registry and checkpoint wire support for the superblock crate.

mod append_log;
mod application;
mod capability;
mod change;
mod error;
mod format;
mod registry;
mod snapshot;
mod state;
mod superblock;
mod transaction;

/// Converts an append-log `block_seq` into the superblock record sequence.
pub fn record_seq_from_block_seq(
    block_seq: u64,
) -> Result<pi_block_device::RecordSeq, error::InvalidBlockSequence> {
    if block_seq == 0 || block_seq > u32::MAX as u64 - 1 {
        return Err(error::InvalidBlockSequence);
    }
    pi_block_device::RecordSeq::new(block_seq as u32).map_err(|_| error::InvalidBlockSequence)
}

#[doc(hidden)]
pub use append_log::{
    DecodedSuperblockRecord, DurableAppendOutcome, SuperblockAppendLog, build_recovering,
    decode_append_log_block, encode_append_log_block,
};
#[doc(hidden)]
pub use append_log::{MaintenanceError, MaintenanceOutcome};
pub use application::{
    ApplicationApplyContext, ApplicationCodec, ApplicationEncoder, ApplicationState, BindingName,
    DeviceLabel, RootVersion,
};
pub use capability::{CapabilityField, CapabilityMismatchError, CapabilityValue};
#[doc(hidden)]
pub use change::{SystemChange, TransactionChange};
pub use error::{
    ApplicationApplyError, ApplicationCodecError, ApplicationCodecFailureKind,
    ApplicationCodecOperation, CreateError, CreateFailureKind, InvalidBlockSequence, OpenError,
    RecoveryErrorContext, RecoveryFailureKind, RecoveryOperation, SuperblockError,
};
pub use format::{
    RecordHeader, RecordKind, decode_checkpoint, decode_delta, decode_record_header,
    encode_checkpoint, encode_delta, encode_record_header,
};
pub use registry::{DeviceRegistration, DeviceSlot, DeviceSlotRef, DeviceTable};
pub use snapshot::{SuperblockSnapshot, SuperblockSnapshotApi};
#[doc(hidden)]
pub use state::{RecoveredState, SuperblockState, SuperblockStatus, decode_checkpoint_state};
pub use superblock::{
    DeviceResolver, InitialDevice, ResolveError, Superblock, SuperblockLogConfig,
};
pub use transaction::{AllocatedBlock, BlockWrite, CommitResult, StreamWriteError, Transaction};
