//! Deterministic checkpoint encoding for the static registry and device state.

use rpds::RedBlackTreeMapSync;

use bytes::Bytes;
use pi_block_device::RecordSeq;

use crate::{ApplicationCodec, ApplicationState, SuperblockError};

type CheckpointState<S> = (
    crate::DeviceTable,
    RedBlackTreeMapSync<pi_block_device::BlockRef, RecordSeq>,
    RedBlackTreeMapSync<crate::BindingName, pi_block_device::BlockRef>,
    S,
);

mod implementation;

/// Encodes a deterministic checkpoint body.
pub fn encode_checkpoint<S, C>(
    record_seq: RecordSeq,
    devices: &crate::DeviceTable,
    roots: &RedBlackTreeMapSync<pi_block_device::BlockRef, RecordSeq>,
    bindings: &RedBlackTreeMapSync<crate::BindingName, pi_block_device::BlockRef>,
    application: &S,
    codec: &C,
) -> Result<Bytes, SuperblockError>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
{
    implementation::encode_checkpoint(record_seq, devices, roots, bindings, application, codec)
}

/// Decodes a checkpoint body using the authoritative envelope sequence.
pub fn decode_checkpoint<S, C>(
    record_seq: RecordSeq,
    bytes: Bytes,
    codec: &C,
) -> Result<CheckpointState<S>, SuperblockError>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
{
    implementation::decode_checkpoint(record_seq, bytes, codec)
}
