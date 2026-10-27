use bytes::Bytes;
use pi_block_device::{BlockRef, DeviceKey, LargeBlockId, RecordSeq};

mod segment;

use super::limits::COLLECTION_LIMIT;
use super::wire::Reader;
use crate::{
    ApplicationCodec, ApplicationCodecFailureKind, ApplicationEncoder, ApplicationState,
    BindingName, SuperblockError, SystemChange, TransactionChange,
};

const DELTA_LIMIT: usize = 16 * 1024 * 1024;
const CHANGE_ENVELOPE_LEN: usize = 8;
const BINDING_NAME_LIMIT: usize = 1_048_576;
const SYSTEM_TRANSACTION_TAG: u8 = 1;
const APPLICATION_TRANSACTION_TAG: u8 = 2;
const APPLICATION_CHANGE_TAG: u8 = 0;
const CREATE_ACTIVE_SEGMENT_TAG: u8 = 10;
const ALLOCATE_SEGMENT_BLOCKS_TAG: u8 = 11;
const RELEASE_SEGMENT_BLOCKS_TAG: u8 = 12;
const REMOVE_SEGMENT_TAG: u8 = 13;
const ALLOCATE_LARGE_BLOCK_TAG: u8 = 20;
const RELEASE_LARGE_BLOCK_TAG: u8 = 21;
const REMOVE_LARGE_BLOCK_TAG: u8 = 22;
const ADD_ROOT_TAG: u8 = 30;
const REMOVE_ROOT_TAG: u8 = 31;
const ADD_BINDING_TAG: u8 = 32;
const REMOVE_BINDING_TAG: u8 = 33;

pub fn encode_delta<S, C>(
    record_seq: RecordSeq,
    changes: &[TransactionChange<S::Delta>],
    codec: &C,
) -> Result<Bytes, SuperblockError>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
{
    if record_seq.is_null() {
        return Err(SuperblockError::RecordSeqExhausted);
    }
    if changes.len() > COLLECTION_LIMIT {
        return Err(SuperblockError::RecordTooLarge);
    }
    let count = u32::try_from(changes.len()).map_err(|_| SuperblockError::RecordTooLarge)?;
    preflight_changes(changes)?;

    let mut output = Vec::new();
    output
        .try_reserve_exact(4)
        .map_err(|_| SuperblockError::RecordTooLarge)?;
    output.extend_from_slice(&count.to_le_bytes());
    for change in changes {
        match change {
            TransactionChange::Application(delta) => {
                let mut encoder = ApplicationEncoder::for_delta();
                codec.encode_delta(delta, &mut encoder).map_err(|source| {
                    if source.reason == ApplicationCodecFailureKind::LimitExceeded {
                        SuperblockError::RecordTooLarge
                    } else {
                        SuperblockError::ApplicationCodec { source }
                    }
                })?;
                let payload = encoder.freeze();
                write_change_header(
                    &mut output,
                    APPLICATION_TRANSACTION_TAG,
                    APPLICATION_CHANGE_TAG,
                    payload.len(),
                )?;
                output.extend_from_slice(&payload);
            }
            TransactionChange::System(change) => {
                let checked = checked_system_change(change)?;
                write_change_header(
                    &mut output,
                    SYSTEM_TRANSACTION_TAG,
                    checked.tag,
                    checked.payload_len,
                )?;
                let payload_start = output.len();
                segment_or_other_payload(change, &mut output)?;
                debug_assert_eq!(output.len() - payload_start, checked.payload_len);
            }
        }
    }
    Ok(Bytes::from(output))
}

#[derive(Clone, Copy)]
struct CheckedSystemChange {
    tag: u8,
    payload_len: usize,
}

fn preflight_changes<D>(changes: &[TransactionChange<D>]) -> Result<(), SuperblockError> {
    let mut body_len = 4;
    for change in changes {
        let payload_len = match change {
            TransactionChange::Application(_) => 0,
            TransactionChange::System(change) => checked_system_change(change)?.payload_len,
        };
        body_len = checked_delta_body_len(body_len, payload_len)?;
    }
    Ok(())
}

fn checked_system_change(change: &SystemChange) -> Result<CheckedSystemChange, SuperblockError> {
    match change {
        SystemChange::CreateActiveSegment { .. }
        | SystemChange::AllocateSegmentBlocks { .. }
        | SystemChange::ReleaseSegmentBlocks { .. }
        | SystemChange::RemoveSegment { .. } => segment::checked_change(change),
        SystemChange::AllocateLargeBlock { logical_len, .. } => {
            if *logical_len == 0 {
                return Err(SuperblockError::InvalidState);
            }
            Ok(CheckedSystemChange {
                tag: ALLOCATE_LARGE_BLOCK_TAG,
                payload_len: 20,
            })
        }
        SystemChange::ReleaseLargeBlock { .. } => Ok(CheckedSystemChange {
            tag: RELEASE_LARGE_BLOCK_TAG,
            payload_len: 12,
        }),
        SystemChange::RemoveLargeBlock { .. } => Ok(CheckedSystemChange {
            tag: REMOVE_LARGE_BLOCK_TAG,
            payload_len: 12,
        }),
        SystemChange::AddRoot { .. } => Ok(CheckedSystemChange {
            tag: ADD_ROOT_TAG,
            payload_len: 12,
        }),
        SystemChange::RemoveRoot { .. } => Ok(CheckedSystemChange {
            tag: REMOVE_ROOT_TAG,
            payload_len: 12,
        }),
        SystemChange::AddBinding { name, .. } => Ok(CheckedSystemChange {
            tag: ADD_BINDING_TAG,
            payload_len: checked_binding_payload_len(name, 12)?,
        }),
        SystemChange::RemoveBinding { name } => Ok(CheckedSystemChange {
            tag: REMOVE_BINDING_TAG,
            payload_len: checked_binding_payload_len(name, 0)?,
        }),
    }
}

fn checked_binding_payload_len(
    name: &BindingName,
    block_len: usize,
) -> Result<usize, SuperblockError> {
    let name_len = name.as_bytes().len();
    if name_len > BINDING_NAME_LIMIT {
        return Err(SuperblockError::RecordTooLarge);
    }
    u32::try_from(name_len).map_err(|_| SuperblockError::RecordTooLarge)?;
    4_usize
        .checked_add(name_len)
        .and_then(|length| length.checked_add(block_len))
        .ok_or(SuperblockError::RecordTooLarge)
}

fn write_change_header(
    output: &mut Vec<u8>,
    transaction_tag: u8,
    change_tag: u8,
    payload_len: usize,
) -> Result<(), SuperblockError> {
    let payload_len = u32::try_from(payload_len).map_err(|_| SuperblockError::RecordTooLarge)?;
    let next_len = checked_delta_body_len(output.len(), payload_len as usize)?;
    output
        .try_reserve_exact(next_len - output.len())
        .map_err(|_| SuperblockError::RecordTooLarge)?;
    output.push(transaction_tag);
    output.push(change_tag);
    output.extend_from_slice(&0_u16.to_le_bytes());
    output.extend_from_slice(&payload_len.to_le_bytes());
    Ok(())
}

fn segment_or_other_payload(
    change: &SystemChange,
    output: &mut Vec<u8>,
) -> Result<(), SuperblockError> {
    match change {
        SystemChange::CreateActiveSegment { .. }
        | SystemChange::AllocateSegmentBlocks { .. }
        | SystemChange::ReleaseSegmentBlocks { .. }
        | SystemChange::RemoveSegment { .. } => segment::write_change(change, output),
        SystemChange::AllocateLargeBlock {
            device,
            id,
            logical_len,
        } => {
            output.extend_from_slice(&device.raw().to_le_bytes());
            output.extend_from_slice(&id.raw().to_le_bytes());
            output.extend_from_slice(&logical_len.to_le_bytes());
            Ok(())
        }
        SystemChange::ReleaseLargeBlock { device, id }
        | SystemChange::RemoveLargeBlock { device, id } => {
            output.extend_from_slice(&device.raw().to_le_bytes());
            output.extend_from_slice(&id.raw().to_le_bytes());
            Ok(())
        }
        SystemChange::AddRoot { block } | SystemChange::RemoveRoot { block } => {
            output.extend_from_slice(&block.to_le_bytes());
            Ok(())
        }
        SystemChange::AddBinding { name, block } => {
            write_name(name, output)?;
            output.extend_from_slice(&block.to_le_bytes());
            Ok(())
        }
        SystemChange::RemoveBinding { name } => write_name(name, output),
    }
}

fn write_name(name: &BindingName, output: &mut Vec<u8>) -> Result<(), SuperblockError> {
    let name_len =
        u32::try_from(name.as_bytes().len()).map_err(|_| SuperblockError::RecordTooLarge)?;
    output.extend_from_slice(&name_len.to_le_bytes());
    output.extend_from_slice(name.as_bytes());
    Ok(())
}

pub fn decode_delta<S, C>(
    bytes: Bytes,
    codec: &C,
) -> Result<Vec<TransactionChange<S::Delta>>, SuperblockError>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
{
    let mut reader = Reader::new(bytes.as_ref());
    let count = reader.u32().map_err(|_| SuperblockError::InvalidState)? as usize;
    if count > COLLECTION_LIMIT {
        return Err(SuperblockError::RecordTooLarge);
    }
    let minimum_changes_len = count
        .checked_mul(CHANGE_ENVELOPE_LEN)
        .ok_or(SuperblockError::InvalidState)?;
    if reader.remaining_bytes().len() < minimum_changes_len {
        return Err(SuperblockError::InvalidState);
    }
    let mut changes = Vec::new();
    changes
        .try_reserve_exact(count)
        .map_err(|_| SuperblockError::RecordTooLarge)?;
    for _ in 0..count {
        let transaction_tag = reader.u8().map_err(|_| SuperblockError::InvalidState)?;
        let change_tag = reader.u8().map_err(|_| SuperblockError::InvalidState)?;
        let flags = reader.u16().map_err(|_| SuperblockError::InvalidState)?;
        let payload_len = reader.u32().map_err(|_| SuperblockError::InvalidState)? as usize;
        if flags != 0 {
            return Err(SuperblockError::InvalidState);
        }
        if transaction_tag == APPLICATION_TRANSACTION_TAG && payload_len > DELTA_LIMIT {
            return Err(SuperblockError::RecordTooLarge);
        }
        if payload_len > reader.remaining_bytes().len() {
            return Err(SuperblockError::InvalidState);
        }
        let payload = reader
            .bytes(payload_len)
            .map_err(|_| SuperblockError::InvalidState)?;
        let change = match transaction_tag {
            APPLICATION_TRANSACTION_TAG => {
                if change_tag != APPLICATION_CHANGE_TAG {
                    return Err(SuperblockError::InvalidState);
                }
                let delta = codec
                    .decode_delta(Bytes::copy_from_slice(payload))
                    .map_err(|source| SuperblockError::ApplicationCodec { source })?;
                TransactionChange::Application(delta)
            }
            SYSTEM_TRANSACTION_TAG => {
                TransactionChange::System(decode_system_change(change_tag, payload)?)
            }
            _ => return Err(SuperblockError::InvalidState),
        };
        changes.push(change);
    }
    if !reader.remaining_bytes().is_empty() {
        return Err(SuperblockError::InvalidState);
    }
    Ok(changes)
}

fn decode_system_change(tag: u8, payload: &[u8]) -> Result<SystemChange, SuperblockError> {
    if let Some(change) = segment::decode_change(tag, payload)? {
        return Ok(change);
    }
    match tag {
        ADD_ROOT_TAG | REMOVE_ROOT_TAG => {
            if payload.len() != 12 {
                return Err(SuperblockError::InvalidState);
            }
            let raw: [u8; 12] = payload
                .try_into()
                .map_err(|_| SuperblockError::InvalidState)?;
            let block = BlockRef::from_le_bytes(raw).map_err(|_| SuperblockError::InvalidState)?;
            if tag == ADD_ROOT_TAG {
                Ok(SystemChange::AddRoot { block })
            } else {
                Ok(SystemChange::RemoveRoot { block })
            }
        }
        ALLOCATE_LARGE_BLOCK_TAG => {
            if payload.len() != 20 {
                return Err(SuperblockError::InvalidState);
            }
            let mut reader = Reader::new(payload);
            let device = DeviceKey::new(reader.u32().map_err(|_| SuperblockError::InvalidState)?)
                .map_err(|_| SuperblockError::InvalidState)?;
            let id = LargeBlockId::new(reader.u64().map_err(|_| SuperblockError::InvalidState)?)
                .map_err(|_| SuperblockError::InvalidState)?;
            let logical_len = reader.u64().map_err(|_| SuperblockError::InvalidState)?;
            if logical_len == 0 || !reader.remaining_bytes().is_empty() {
                return Err(SuperblockError::InvalidState);
            }
            Ok(SystemChange::AllocateLargeBlock {
                device,
                id,
                logical_len,
            })
        }
        RELEASE_LARGE_BLOCK_TAG | REMOVE_LARGE_BLOCK_TAG => {
            if payload.len() != 12 {
                return Err(SuperblockError::InvalidState);
            }
            let mut reader = Reader::new(payload);
            let device = DeviceKey::new(reader.u32().map_err(|_| SuperblockError::InvalidState)?)
                .map_err(|_| SuperblockError::InvalidState)?;
            let id = LargeBlockId::new(reader.u64().map_err(|_| SuperblockError::InvalidState)?)
                .map_err(|_| SuperblockError::InvalidState)?;
            if !reader.remaining_bytes().is_empty() {
                return Err(SuperblockError::InvalidState);
            }
            if tag == RELEASE_LARGE_BLOCK_TAG {
                Ok(SystemChange::ReleaseLargeBlock { device, id })
            } else {
                Ok(SystemChange::RemoveLargeBlock { device, id })
            }
        }
        ADD_BINDING_TAG => {
            let (name, block) = decode_binding_payload(payload, true)?;
            Ok(SystemChange::AddBinding {
                name,
                block: block.ok_or(SuperblockError::InvalidState)?,
            })
        }
        REMOVE_BINDING_TAG => {
            let (name, block) = decode_binding_payload(payload, false)?;
            if block.is_some() {
                return Err(SuperblockError::InvalidState);
            }
            Ok(SystemChange::RemoveBinding { name })
        }
        _ => Err(SuperblockError::InvalidState),
    }
}

fn decode_binding_payload(
    payload: &[u8],
    has_block: bool,
) -> Result<(BindingName, Option<BlockRef>), SuperblockError> {
    let block_len = if has_block { 12 } else { 0 };
    if payload.len() < 4 + 1 + block_len {
        return Err(SuperblockError::InvalidState);
    }
    let mut reader = Reader::new(payload);
    let name_len = reader.u32().map_err(|_| SuperblockError::InvalidState)? as usize;
    if name_len == 0 {
        return Err(SuperblockError::InvalidState);
    }
    if name_len > BINDING_NAME_LIMIT {
        return Err(SuperblockError::RecordTooLarge);
    }
    let required = 4_usize
        .checked_add(name_len)
        .and_then(|value| value.checked_add(block_len))
        .ok_or(SuperblockError::InvalidState)?;
    if required != payload.len() {
        return Err(SuperblockError::InvalidState);
    }
    let name_bytes = reader
        .bytes(name_len)
        .map_err(|_| SuperblockError::InvalidState)?
        .to_vec();
    let name = BindingName::new(name_bytes).map_err(|_| SuperblockError::InvalidState)?;
    let block = if has_block {
        Some(
            BlockRef::from_le_bytes(
                reader
                    .array_12()
                    .map_err(|_| SuperblockError::InvalidState)?,
            )
            .map_err(|_| SuperblockError::InvalidState)?,
        )
    } else {
        None
    };
    if !reader.remaining_bytes().is_empty() {
        return Err(SuperblockError::InvalidState);
    }
    Ok((name, block))
}

pub(crate) fn checked_delta_body_len(
    current: usize,
    payload: usize,
) -> Result<usize, SuperblockError> {
    current
        .checked_add(8)
        .and_then(|length| length.checked_add(payload))
        .filter(|&length| length <= u32::MAX as usize)
        .ok_or(SuperblockError::RecordTooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_delta_body_len_accepts_exact_u32_max() {
        let current = u32::MAX as usize - 8;
        let result = checked_delta_body_len(current, 0);
        assert_eq!(
            result.expect("exact u32::MAX length should be accepted"),
            u32::MAX as usize
        );
    }

    #[test]
    fn checked_delta_body_len_rejects_length_above_u32_max() {
        let current = u32::MAX as usize - 8;
        let result = checked_delta_body_len(current, 1);
        assert!(matches!(result, Err(SuperblockError::RecordTooLarge)));
    }

    #[test]
    fn checked_delta_body_len_rejects_checked_add_overflow() {
        let result = checked_delta_body_len(usize::MAX, 0);
        assert!(matches!(result, Err(SuperblockError::RecordTooLarge)));
    }
}
