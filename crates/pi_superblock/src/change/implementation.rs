//! Private ordered system-change replay over persistent collections.

use super::*;

pub(super) fn apply_system_change<S>(
    state: &mut crate::SuperblockState<S>,
    change: &SystemChange,
    record_seq: pi_block_device::RecordSeq,
) -> Result<(), crate::SuperblockError>
where
    S: crate::ApplicationState,
{
    use crate::SuperblockError::InvalidBlockRef;
    use pi_block_device::{AllocationRun, BlockId, BlockRef, LargeBlockData, SegmentData};
    if record_seq.is_null() || record_seq < state.record_seq {
        return Err(InvalidBlockRef);
    }
    if state.devices.iter().any(|slot| {
        !slot.state().validate_at(record_seq)
            || !crate::state::allocations_are_not_in_the_future(slot.state(), record_seq)
    }) {
        return Err(InvalidBlockRef);
    }
    // Stage persistent paths locally. A failed change never leaks partial mutation.
    let mut next = state.clone();
    match change {
        SystemChange::AddRoot { block } => {
            if next.roots.contains_key(block)
                || !crate::state::is_live_block(&next.devices, *block, record_seq)
            {
                return Err(InvalidBlockRef);
            }
            next.roots.insert_mut(*block, record_seq);
        }
        SystemChange::RemoveRoot { block } => {
            if !next.roots.contains_key(block) || next.bindings.values().any(|value| value == block)
            {
                return Err(InvalidBlockRef);
            }
            next.roots.remove_mut(block);
        }
        SystemChange::AddBinding { name, block } => {
            if next.bindings.contains_key(name) || !next.roots.contains_key(block) {
                return Err(InvalidBlockRef);
            }
            next.bindings.insert_mut(name.clone(), *block);
        }
        SystemChange::RemoveBinding { name } => {
            if !next.bindings.contains_key(name) {
                return Err(InvalidBlockRef);
            }
            next.bindings.remove_mut(name);
        }
        SystemChange::AllocateLargeBlock {
            device,
            id,
            logical_len,
        } => {
            let device = next.devices.state_mut(*device).ok_or(InvalidBlockRef)?;
            let (expected, cursor) = device
                .large_blocks
                .next_id
                .next_id()
                .map_err(|_| InvalidBlockRef)?;
            if expected != *id || *logical_len == 0 || device.large_blocks.entries.contains_key(id)
            {
                return Err(InvalidBlockRef);
            }
            device.large_blocks.entries.insert_mut(
                *id,
                LargeBlockData {
                    allocated_at: record_seq,
                    released_at: pi_block_device::RecordSeq::NULL,
                    logical_len: *logical_len,
                },
            );
            device.large_blocks.next_id = cursor;
        }
        SystemChange::ReleaseLargeBlock { device, id }
        | SystemChange::RemoveLargeBlock { device, id } => {
            let block = BlockRef {
                device: *device,
                block: BlockId::large(*id),
            };
            if next.roots.contains_key(&block)
                || next.bindings.values().any(|value| *value == block)
            {
                return Err(InvalidBlockRef);
            }
            let device = next.devices.state_mut(*device).ok_or(InvalidBlockRef)?;
            let data = device
                .large_blocks
                .entries
                .get_mut(id)
                .ok_or(InvalidBlockRef)?;
            if matches!(change, SystemChange::ReleaseLargeBlock { .. }) {
                if !data.released_at.is_null() || data.allocated_at >= record_seq {
                    return Err(InvalidBlockRef);
                }
                data.released_at = record_seq;
            } else {
                if data.released_at.is_null() || data.released_at > record_seq {
                    return Err(InvalidBlockRef);
                }
                device.large_blocks.entries.remove_mut(id);
            }
        }
        SystemChange::CreateActiveSegment {
            device,
            level,
            version,
        } => {
            let device = next.devices.state_mut(*device).ok_or(InvalidBlockRef)?;
            let level = device
                .levels
                .get_mut(*level as usize)
                .ok_or(InvalidBlockRef)?;
            let expected = u32::try_from(level.versions.len())
                .ok()
                .and_then(|len| level.min_version.raw().checked_add(len))
                .ok_or(InvalidBlockRef)?;
            if expected != version.raw()
                || !level.validate()
                || level
                    .versions
                    .last()
                    .and_then(Option::as_ref)
                    .is_none_or(|segment| segment.allocation_runs.is_empty())
            {
                return Err(InvalidBlockRef);
            }
            level.versions.push_back_mut(Some(SegmentData {
                allocation_runs: rpds::VectorSync::new_sync(),
                released_at: rpds::VectorSync::new_sync(),
            }));
        }
        SystemChange::AllocateSegmentBlocks {
            device,
            level,
            version,
            len,
        } => {
            let device = next.devices.state_mut(*device).ok_or(InvalidBlockRef)?;
            let level = device
                .levels
                .get_mut(*level as usize)
                .ok_or(InvalidBlockRef)?;
            let offset = version
                .raw()
                .checked_sub(level.min_version.raw())
                .ok_or(InvalidBlockRef)? as usize;
            if *len == 0 || offset.checked_add(1) != Some(level.versions.len()) {
                return Err(InvalidBlockRef);
            }
            let data = level
                .versions
                .get_mut(offset)
                .and_then(Option::as_mut)
                .ok_or(InvalidBlockRef)?;
            let last = data.allocation_runs.len().checked_sub(1);
            if let Some(index) = last {
                let run = data.allocation_runs.get_mut(index).ok_or(InvalidBlockRef)?;
                if run.allocated_at > record_seq {
                    return Err(InvalidBlockRef);
                }
                if run.allocated_at == record_seq {
                    run.len = run.len.checked_add(*len).ok_or(InvalidBlockRef)?;
                } else {
                    data.allocation_runs.push_back_mut(AllocationRun {
                        allocated_at: record_seq,
                        len: *len,
                    });
                }
            } else {
                data.allocation_runs.push_back_mut(AllocationRun {
                    allocated_at: record_seq,
                    len: *len,
                });
            }
            let total = (data.released_at.len() as u64)
                .checked_add(u64::from(*len))
                .ok_or(InvalidBlockRef)?;
            if total > u64::from(u32::MAX) + 1 || usize::try_from(total).is_err() {
                return Err(InvalidBlockRef);
            }
            for _ in 0..*len {
                data.released_at
                    .push_back_mut(pi_block_device::RecordSeq::NULL);
            }
        }
        SystemChange::ReleaseSegmentBlocks {
            device,
            level,
            version,
            ranges,
        } => {
            let slot = next.devices.get(*device).ok_or(InvalidBlockRef)?;
            let window = slot
                .state()
                .levels
                .get(*level as usize)
                .ok_or(InvalidBlockRef)?;
            let offset = version
                .raw()
                .checked_sub(window.min_version.raw())
                .ok_or(InvalidBlockRef)? as usize;
            let segment = window
                .versions
                .get(offset)
                .and_then(Option::as_ref)
                .ok_or(InvalidBlockRef)?;
            if ranges.iter().any(|range| {
                range
                    .start
                    .checked_add(range.len.get())
                    .is_none_or(|end| end as u64 > segment.released_at.len() as u64)
            }) {
                return Err(InvalidBlockRef);
            }
            let mut end = 0;
            for range in ranges {
                if range.start < end {
                    return Err(InvalidBlockRef);
                }
                end = range
                    .start
                    .checked_add(range.len.get())
                    .ok_or(InvalidBlockRef)?;
                for index in range.start..end {
                    let block = BlockRef {
                        device: *device,
                        block: BlockId::ordinary(*level, *version, index)
                            .map_err(|_| InvalidBlockRef)?,
                    };
                    if next.roots.contains_key(&block)
                        || next.bindings.values().any(|value| *value == block)
                    {
                        return Err(InvalidBlockRef);
                    }
                }
            }
            let device = next.devices.state_mut(*device).ok_or(InvalidBlockRef)?;
            let level = device
                .levels
                .get_mut(*level as usize)
                .ok_or(InvalidBlockRef)?;
            let offset = version
                .raw()
                .checked_sub(level.min_version.raw())
                .ok_or(InvalidBlockRef)? as usize;
            let data = level
                .versions
                .get_mut(offset)
                .and_then(Option::as_mut)
                .ok_or(InvalidBlockRef)?;
            for range in ranges {
                let end = range
                    .start
                    .checked_add(range.len.get())
                    .ok_or(InvalidBlockRef)?;
                for index in range.start..end {
                    let mut allocated = None;
                    let mut run_end = 0_u64;
                    for run in &data.allocation_runs {
                        run_end += u64::from(run.len);
                        if u64::from(index) < run_end {
                            allocated = Some(run.allocated_at);
                            break;
                        }
                    }
                    if allocated.is_none_or(|seq| seq >= record_seq) {
                        return Err(InvalidBlockRef);
                    }
                    let released = data
                        .released_at
                        .get_mut(index as usize)
                        .ok_or(InvalidBlockRef)?;
                    if !released.is_null() {
                        return Err(InvalidBlockRef);
                    }
                    *released = record_seq;
                }
            }
        }
        SystemChange::RemoveSegment {
            device,
            level,
            version,
        } => {
            let device = next.devices.state_mut(*device).ok_or(InvalidBlockRef)?;
            let level = device
                .levels
                .get_mut(*level as usize)
                .ok_or(InvalidBlockRef)?;
            let offset = version
                .raw()
                .checked_sub(level.min_version.raw())
                .ok_or(InvalidBlockRef)? as usize;
            if offset
                .checked_add(1)
                .is_none_or(|end| end >= level.versions.len())
            {
                return Err(InvalidBlockRef);
            }
            let data = level
                .versions
                .get(offset)
                .and_then(Option::as_ref)
                .ok_or(InvalidBlockRef)?;
            if data.released_at.is_empty()
                || data
                    .released_at
                    .iter()
                    .any(|seq| seq.is_null() || *seq > record_seq)
            {
                return Err(InvalidBlockRef);
            }
            *level.versions.get_mut(offset).ok_or(InvalidBlockRef)? = None;
            let trim = level
                .versions
                .iter()
                .take_while(|entry| entry.is_none())
                .count();
            if trim > 0 {
                level.min_version = pi_block_device::SegmentVersion::new(
                    level
                        .min_version
                        .raw()
                        .checked_add(u32::try_from(trim).map_err(|_| InvalidBlockRef)?)
                        .ok_or(InvalidBlockRef)?,
                )
                .map_err(|_| InvalidBlockRef)?;
                level.versions = level.versions.iter().skip(trim).cloned().collect();
            }
        }
    }
    if next.devices.iter().any(|slot| {
        !slot.state().validate_at(record_seq)
            || !crate::state::allocations_are_not_in_the_future(slot.state(), record_seq)
    }) {
        return Err(InvalidBlockRef);
    }
    *state = next;
    Ok(())
}
