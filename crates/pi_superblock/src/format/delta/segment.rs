use std::num::NonZeroU32;

use pi_block_device::{BlockRange, DeviceKey, SegmentVersion};

use super::{
    ALLOCATE_SEGMENT_BLOCKS_TAG, CREATE_ACTIVE_SEGMENT_TAG, CheckedSystemChange,
    RELEASE_SEGMENT_BLOCKS_TAG, REMOVE_SEGMENT_TAG,
};
use crate::{SuperblockError, SystemChange, format::limits::COLLECTION_LIMIT};

pub(super) fn checked_change(
    change: &SystemChange,
) -> Result<CheckedSystemChange, SuperblockError> {
    let (tag, payload_len) = match change {
        SystemChange::CreateActiveSegment {
            device,
            level,
            version,
        } => {
            check_header(*device, *level, *version)?;
            (CREATE_ACTIVE_SEGMENT_TAG, 12)
        }
        SystemChange::AllocateSegmentBlocks {
            device,
            level,
            version,
            len,
        } => {
            check_header(*device, *level, *version)?;
            NonZeroU32::new(*len).ok_or(SuperblockError::InvalidState)?;
            (ALLOCATE_SEGMENT_BLOCKS_TAG, 16)
        }
        SystemChange::ReleaseSegmentBlocks {
            device,
            level,
            version,
            ranges,
        } => {
            check_header(*device, *level, *version)?;
            if ranges.len() > COLLECTION_LIMIT {
                return Err(SuperblockError::RecordTooLarge);
            }
            validate_ranges(ranges)?;
            (
                RELEASE_SEGMENT_BLOCKS_TAG,
                checked_range_payload_len(ranges.len())?,
            )
        }
        SystemChange::RemoveSegment {
            device,
            level,
            version,
        } => {
            check_header(*device, *level, *version)?;
            (REMOVE_SEGMENT_TAG, 12)
        }
        _ => return Err(SuperblockError::InvalidState),
    };
    Ok(CheckedSystemChange { tag, payload_len })
}

pub(super) fn write_change(
    change: &SystemChange,
    output: &mut Vec<u8>,
) -> Result<(), SuperblockError> {
    checked_change(change)?;
    match change {
        SystemChange::CreateActiveSegment {
            device,
            level,
            version,
        }
        | SystemChange::RemoveSegment {
            device,
            level,
            version,
        } => {
            output.extend_from_slice(&encode_header(*device, *level, *version)?);
        }
        SystemChange::AllocateSegmentBlocks {
            device,
            level,
            version,
            len,
        } => {
            output.extend_from_slice(&encode_header(*device, *level, *version)?);
            output.extend_from_slice(&len.to_le_bytes());
        }
        SystemChange::ReleaseSegmentBlocks {
            device,
            level,
            version,
            ranges,
        } => {
            output.extend_from_slice(&encode_header(*device, *level, *version)?);
            let count = u32::try_from(ranges.len()).map_err(|_| SuperblockError::RecordTooLarge)?;
            output.extend_from_slice(&count.to_le_bytes());
            for range in ranges {
                output.extend_from_slice(&range.start.to_le_bytes());
                output.extend_from_slice(&range.len.get().to_le_bytes());
            }
        }
        _ => return Err(SuperblockError::InvalidState),
    }
    Ok(())
}

pub(super) fn decode_change(
    tag: u8,
    payload: &[u8],
) -> Result<Option<SystemChange>, SuperblockError> {
    let change = match tag {
        CREATE_ACTIVE_SEGMENT_TAG | REMOVE_SEGMENT_TAG => {
            let (device, level, version) = decode_header(payload)?;
            if tag == CREATE_ACTIVE_SEGMENT_TAG {
                SystemChange::CreateActiveSegment {
                    device,
                    level,
                    version,
                }
            } else {
                SystemChange::RemoveSegment {
                    device,
                    level,
                    version,
                }
            }
        }
        ALLOCATE_SEGMENT_BLOCKS_TAG => {
            if payload.len() != 16 {
                return Err(SuperblockError::InvalidState);
            }
            let (device, level, version) = decode_header(&payload[..12])?;
            let len = u32::from_le_bytes(
                payload[12..16]
                    .try_into()
                    .map_err(|_| SuperblockError::InvalidState)?,
            );
            NonZeroU32::new(len).ok_or(SuperblockError::InvalidState)?;
            SystemChange::AllocateSegmentBlocks {
                device,
                level,
                version,
                len,
            }
        }
        RELEASE_SEGMENT_BLOCKS_TAG => decode_release(payload)?,
        _ => return Ok(None),
    };
    Ok(Some(change))
}

fn check_header(
    _device: DeviceKey,
    level: u8,
    _version: SegmentVersion,
) -> Result<(), SuperblockError> {
    if level >= 15 {
        return Err(SuperblockError::InvalidState);
    }
    Ok(())
}

fn encode_header(
    device: DeviceKey,
    level: u8,
    version: SegmentVersion,
) -> Result<[u8; 12], SuperblockError> {
    check_header(device, level, version)?;
    let mut payload = [0_u8; 12];
    payload[..4].copy_from_slice(&device.raw().to_le_bytes());
    payload[4] = level;
    payload[8..].copy_from_slice(&version.raw().to_le_bytes());
    Ok(payload)
}

fn decode_header(payload: &[u8]) -> Result<(DeviceKey, u8, SegmentVersion), SuperblockError> {
    if payload.len() != 12 || payload[5..8] != [0_u8; 3] {
        return Err(SuperblockError::InvalidState);
    }
    let device = DeviceKey::new(u32::from_le_bytes(
        payload[..4]
            .try_into()
            .map_err(|_| SuperblockError::InvalidState)?,
    ))
    .map_err(|_| SuperblockError::InvalidState)?;
    let level = payload[4];
    let version = SegmentVersion::new(u32::from_le_bytes(
        payload[8..12]
            .try_into()
            .map_err(|_| SuperblockError::InvalidState)?,
    ))
    .map_err(|_| SuperblockError::InvalidState)?;
    check_header(device, level, version)?;
    Ok((device, level, version))
}

fn checked_range_payload_len(range_count: usize) -> Result<usize, SuperblockError> {
    range_count
        .checked_mul(8)
        .and_then(|length| length.checked_add(16))
        .filter(|&length| length <= u32::MAX as usize)
        .ok_or(SuperblockError::RecordTooLarge)
}

fn validate_ranges(ranges: &[BlockRange]) -> Result<(), SuperblockError> {
    let mut previous: Option<(u32, u32)> = None;
    for range in ranges {
        let end = range
            .start
            .checked_add(range.len.get())
            .ok_or(SuperblockError::InvalidState)?;
        if previous
            .is_some_and(|(start, previous_end)| range.start <= start || range.start < previous_end)
        {
            return Err(SuperblockError::InvalidState);
        }
        previous = Some((range.start, end));
    }
    Ok(())
}

fn decode_release(payload: &[u8]) -> Result<SystemChange, SuperblockError> {
    if payload.len() < 16 {
        return Err(SuperblockError::InvalidState);
    }
    let range_count = u32::from_le_bytes(
        payload[12..16]
            .try_into()
            .map_err(|_| SuperblockError::InvalidState)?,
    ) as usize;
    if range_count > COLLECTION_LIMIT {
        return Err(SuperblockError::RecordTooLarge);
    }
    if payload.len() != checked_range_payload_len(range_count)? {
        return Err(SuperblockError::InvalidState);
    }
    let (device, level, version) = decode_header(&payload[..12])?;
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(range_count)
        .map_err(|_| SuperblockError::RecordTooLarge)?;
    for bytes in payload[16..].chunks_exact(8) {
        let start = u32::from_le_bytes(
            bytes[..4]
                .try_into()
                .map_err(|_| SuperblockError::InvalidState)?,
        );
        let len = NonZeroU32::new(u32::from_le_bytes(
            bytes[4..]
                .try_into()
                .map_err(|_| SuperblockError::InvalidState)?,
        ))
        .ok_or(SuperblockError::InvalidState)?;
        ranges.push(BlockRange { start, len });
    }
    validate_ranges(&ranges)?;
    Ok(SystemChange::ReleaseSegmentBlocks {
        device,
        level,
        version,
        ranges,
    })
}
