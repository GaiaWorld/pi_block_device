//! Deterministic checkpoint encoding for the static registry and device state.

use rpds::RedBlackTreeMapSync;

use bytes::Bytes;
use pi_block_device::{BlockDeviceState, LevelVersions, RecordSeq, SegmentData};

use super::super::limits::{CHECKPOINT_LIMIT, COLLECTION_LIMIT};
use super::super::wire::Reader;
use crate::{
    ApplicationCodec, ApplicationEncoder, ApplicationState, DeviceRegistration, SuperblockError,
};

use super::CheckpointState;

/// Encodes a deterministic checkpoint body.
pub(super) fn encode_checkpoint<S, C>(
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
    validate_state(record_seq, devices, roots, bindings)?;
    let mut output = ApplicationEncoder::with_checkpoint_limit();
    output
        .put_u32_le(collection_count(devices.len())?)
        .map_err(codec_error)?;
    for (index, slot) in devices.iter().enumerate() {
        put_device_registration(
            &mut output,
            index as u32 + 1,
            slot.registration(),
            slot.state(),
        )?;
    }
    output
        .put_u32_le(collection_count(roots.size())?)
        .map_err(codec_error)?;
    for (block, published) in roots {
        output
            .put_slice(&block.to_le_bytes())
            .map_err(codec_error)?;
        put_lifecycle(&mut output, *published)?;
    }
    output
        .put_u32_le(collection_count(bindings.size())?)
        .map_err(codec_error)?;
    for (name, block) in bindings {
        output
            .put_u32_le(name.as_bytes().len() as u32)
            .map_err(codec_error)?;
        output.put_slice(name.as_bytes()).map_err(codec_error)?;
        output
            .put_slice(&block.to_le_bytes())
            .map_err(codec_error)?;
    }
    let mut app = ApplicationEncoder::for_state();
    codec
        .encode_state(application, &mut app)
        .map_err(codec_error)?;
    output.put_slice(&app.freeze()).map_err(codec_error)?;
    Ok(output.freeze())
}

/// Decodes a checkpoint body using the envelope sequence as authoritative.
pub(super) fn decode_checkpoint<S, C>(
    record_seq: RecordSeq,
    bytes: Bytes,
    codec: &C,
) -> Result<CheckpointState<S>, SuperblockError>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
{
    if record_seq.is_null() {
        return Err(SuperblockError::InvalidBlockRef);
    }
    let mut reader = Reader::new(&bytes);
    let count = read_count(&mut reader, 1)?;
    if count == 0 {
        return Err(SuperblockError::InvalidBlockRef);
    }
    let mut slots = Vec::new();
    for index in 0..count {
        if reader.u32()? != index as u32 + 1 {
            return Err(SuperblockError::InvalidBlockRef);
        }
        let id = pi_block_device::BlockDeviceId::from_bytes(reader.array_16()?)
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        let length = reader.u16()? as usize;
        if length == 0 || length > 255 {
            return Err(SuperblockError::InvalidBlockRef);
        }
        let label = std::str::from_utf8(reader.bytes(length)?)
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        let label = crate::DeviceLabel::new(label.to_owned())
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        let state = decode_device_state(&mut reader, record_seq)?;
        slots.push(crate::DeviceSlot::new(
            DeviceRegistration { id, label },
            state,
        ));
    }
    let devices = crate::DeviceTable::new(slots).map_err(|_| SuperblockError::InvalidBlockRef)?;
    let mut roots = RedBlackTreeMapSync::new_sync();
    let mut previous = None;
    for _ in 0..read_count(&mut reader, 16)? {
        let block = pi_block_device::BlockRef::from_le_bytes(reader.array_12()?)
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        if previous.is_some_and(|prev| prev >= block) {
            return Err(SuperblockError::InvalidBlockRef);
        }
        previous = Some(block);
        roots.insert_mut(block, lifecycle(reader.u32()?)?);
    }
    let mut bindings = RedBlackTreeMapSync::new_sync();
    let mut previous: Option<crate::BindingName> = None;
    for _ in 0..read_count(&mut reader, 4)? {
        let length = reader.u32()? as usize;
        if length > 1_048_576 {
            return Err(SuperblockError::RecordTooLarge);
        }
        let name = crate::BindingName::new(reader.bytes(length)?.to_vec())
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        if previous.as_ref().is_some_and(|prev| prev >= &name) {
            return Err(SuperblockError::InvalidBlockRef);
        }
        previous = Some(name.clone());
        let block = pi_block_device::BlockRef::from_le_bytes(reader.array_12()?)
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        bindings.insert_mut(name, block);
    }
    validate_state(record_seq, &devices, &roots, &bindings)?;
    let remainder = reader.remaining_bytes();
    if remainder.len() > crate::application::STATE_LIMIT {
        return Err(SuperblockError::RecordTooLarge);
    }
    // The application codec owns its self-describing payload and validates the entire remainder.
    let application = codec
        .decode_state(bytes.slice(bytes.len() - remainder.len()..))
        .map_err(codec_error)?;
    Ok((devices, roots, bindings, application))
}

fn codec_error(source: crate::ApplicationCodecError) -> SuperblockError {
    if source.reason == crate::ApplicationCodecFailureKind::LimitExceeded {
        SuperblockError::RecordTooLarge
    } else {
        SuperblockError::ApplicationCodec { source }
    }
}

fn put_device_registration(
    output: &mut ApplicationEncoder,
    key: u32,
    registration: &DeviceRegistration,
    state: &BlockDeviceState,
) -> Result<(), SuperblockError> {
    output.put_u32_le(key).map_err(codec_error)?;
    output
        .put_slice(registration.id.as_bytes())
        .map_err(codec_error)?;
    output
        .put_u16_le(registration.label.as_bytes().len() as u16)
        .map_err(codec_error)?;
    output
        .put_slice(registration.label.as_bytes())
        .map_err(codec_error)?;
    put_device_state(output, state)
}

fn put_device_state(
    output: &mut ApplicationEncoder,
    state: &BlockDeviceState,
) -> Result<(), SuperblockError> {
    for level in &state.levels {
        put_level(output, level)?;
    }
    output
        .put_u64_le(state.large_blocks.next_id.raw())
        .map_err(codec_error)?;
    output
        .put_u32_le(collection_count(state.large_blocks.entries.size())?)
        .map_err(codec_error)?;
    for (id, data) in &state.large_blocks.entries {
        output.put_u64_le(id.raw()).map_err(codec_error)?;
        put_lifecycle(output, data.allocated_at)?;
        put_lifecycle(output, data.released_at)?;
        output.put_u64_le(data.logical_len).map_err(codec_error)?;
    }
    Ok(())
}

fn put_level(
    output: &mut ApplicationEncoder,
    level: &LevelVersions,
) -> Result<(), SuperblockError> {
    let version_count = collection_count(level.versions.len())?;
    output
        .put_u32_le(level.min_version.raw())
        .map_err(codec_error)?;
    output.put_u32_le(version_count).map_err(codec_error)?;
    for entry in &level.versions {
        match entry {
            None => output.put_u8(0).map_err(codec_error)?,
            Some(data) => {
                output.put_u8(1).map_err(codec_error)?;
                put_segment(output, data)?;
            }
        }
    }
    Ok(())
}

fn put_segment(output: &mut ApplicationEncoder, data: &SegmentData) -> Result<(), SuperblockError> {
    let run_count = collection_count(data.allocation_runs.len())?;
    let release_count = collection_count(data.released_at.len())?;
    output.put_u32_le(run_count).map_err(codec_error)?;
    for run in &data.allocation_runs {
        put_lifecycle(output, run.allocated_at)?;
        output.put_u32_le(run.len).map_err(codec_error)?;
    }
    output.put_u32_le(release_count).map_err(codec_error)?;
    for released_at in &data.released_at {
        put_lifecycle(output, *released_at)?;
    }
    Ok(())
}

fn put_lifecycle(output: &mut ApplicationEncoder, value: RecordSeq) -> Result<(), SuperblockError> {
    output.put_u32_le(value.raw()).map_err(codec_error)
}

fn collection_count(length: usize) -> Result<u32, SuperblockError> {
    if length > COLLECTION_LIMIT {
        return Err(SuperblockError::RecordTooLarge);
    }
    u32::try_from(length).map_err(|_| SuperblockError::RecordTooLarge)
}

fn check_device_state_collections(state: &BlockDeviceState) -> Result<(), SuperblockError> {
    collection_count(state.large_blocks.entries.size())?;
    for level in &state.levels {
        collection_count(level.versions.len())?;
        for segment in level.versions.iter().flatten() {
            collection_count(segment.allocation_runs.len())?;
            collection_count(segment.released_at.len())?;
        }
    }
    Ok(())
}

fn decode_device_state(
    reader: &mut Reader<'_>,
    current: RecordSeq,
) -> Result<BlockDeviceState, SuperblockError> {
    let mut state = BlockDeviceState::initial().map_err(|_| SuperblockError::InvalidBlockRef)?;
    for level in &mut state.levels {
        *level = decode_level(reader, current)?;
    }
    state.large_blocks.next_id = pi_block_device::LargeBlockCursor::new(reader.u64()?)
        .map_err(|_| SuperblockError::InvalidBlockRef)?;
    let mut previous = None;
    for _ in 0..read_count(reader, 24)? {
        let id = pi_block_device::LargeBlockId::new(reader.u64()?)
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        if previous.is_some_and(|prev| prev >= id) {
            return Err(SuperblockError::InvalidBlockRef);
        }
        previous = Some(id);
        let data = pi_block_device::LargeBlockData {
            allocated_at: lifecycle(reader.u32()?)?,
            released_at: lifecycle(reader.u32()?)?,
            logical_len: reader.u64()?,
        };
        state.large_blocks.entries.insert_mut(id, data);
    }
    if !state.validate_at(current)
        || !crate::state::allocations_are_not_in_the_future(&state, current)
    {
        return Err(SuperblockError::InvalidBlockRef);
    }
    Ok(state)
}

fn decode_level(
    reader: &mut Reader<'_>,
    current: RecordSeq,
) -> Result<LevelVersions, SuperblockError> {
    let min_version = pi_block_device::SegmentVersion::new(reader.u32()?)
        .map_err(|_| SuperblockError::InvalidBlockRef)?;
    let count = read_count(reader, 1)?;
    let mut versions = rpds::VectorSync::new_sync();
    for _ in 0..count {
        versions.push_back_mut(match reader.u8()? {
            0 => None,
            1 => Some(decode_segment(reader, current)?),
            _ => return Err(SuperblockError::InvalidBlockRef),
        });
    }
    let level = LevelVersions {
        min_version,
        versions,
    };
    if !level.validate_at(current) {
        return Err(SuperblockError::InvalidBlockRef);
    }
    Ok(level)
}

fn decode_segment(
    reader: &mut Reader<'_>,
    current: RecordSeq,
) -> Result<SegmentData, SuperblockError> {
    let mut allocation_runs = rpds::VectorSync::new_sync();
    let mut total = 0_u64;
    for _ in 0..read_count(reader, 8)? {
        let allocated_at = lifecycle(reader.u32()?)?;
        let len = reader.u32()?;
        total += u64::from(len);
        allocation_runs.push_back_mut(pi_block_device::AllocationRun { allocated_at, len });
    }
    let count = read_count(reader, 4)?;
    if total != count as u64 {
        return Err(SuperblockError::InvalidBlockRef);
    }
    let mut released_at = rpds::VectorSync::new_sync();
    for _ in 0..count {
        released_at.push_back_mut(lifecycle(reader.u32()?)?);
    }
    let segment = SegmentData {
        allocation_runs,
        released_at,
    };
    if !segment.validate_at(current) {
        return Err(SuperblockError::InvalidBlockRef);
    }
    Ok(segment)
}

fn read_count(reader: &mut Reader<'_>, minimum: usize) -> Result<usize, SuperblockError> {
    let count = reader.u32()? as usize;
    collection_count(count)?;
    if count
        .checked_mul(minimum)
        .is_none_or(|length| length > reader.remaining_bytes().len())
    {
        return Err(SuperblockError::InvalidBlockRef);
    }
    Ok(count)
}

fn validate_state(
    current: RecordSeq,
    devices: &crate::DeviceTable,
    roots: &RedBlackTreeMapSync<pi_block_device::BlockRef, RecordSeq>,
    bindings: &RedBlackTreeMapSync<crate::BindingName, pi_block_device::BlockRef>,
) -> Result<(), SuperblockError> {
    collection_count(devices.len())?;
    collection_count(roots.size())?;
    collection_count(bindings.size())?;
    if current.is_null() || devices.is_empty() {
        return Err(SuperblockError::InvalidBlockRef);
    }
    for slot in devices.iter() {
        check_device_state_collections(slot.state())?;
        if !slot.state().validate_at(current)
            || !crate::state::allocations_are_not_in_the_future(slot.state(), current)
        {
            return Err(SuperblockError::InvalidBlockRef);
        }
    }
    if roots.iter().any(|(block, published)| {
        published.is_null()
            || *published > current
            || !crate::state::is_live_block(devices, *block, current)
    }) || bindings.values().any(|block| !roots.contains_key(block))
    {
        return Err(SuperblockError::InvalidBlockRef);
    }
    Ok(())
}

fn lifecycle(raw: u32) -> Result<RecordSeq, SuperblockError> {
    RecordSeq::from_lifecycle_raw(raw).map_err(|_| SuperblockError::InvalidBlockRef)
}

impl ApplicationEncoder {
    pub(crate) fn with_checkpoint_limit() -> Self {
        Self::with_limit(CHECKPOINT_LIMIT)
    }
}
