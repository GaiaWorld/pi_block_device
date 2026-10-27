use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use pi_block_device::{
    BlockDeviceState, BlockRange, DeviceKey, LargeBlockId, RetiredObject, SegmentVersion,
};

use super::*;

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub(super) fn release_impl(&mut self, blocks: Vec<BlockRef>) -> Result<(), SuperblockError> {
        self.ensure_open()?;
        let record_seq = self.candidate_seq()?;
        let mut ordinary = BTreeMap::<(DeviceKey, u8, SegmentVersion), BTreeSet<u32>>::new();
        let mut large = BTreeMap::<DeviceKey, BTreeSet<LargeBlockId>>::new();
        let mut unique = BTreeSet::new();

        for block in blocks {
            if !unique.insert(block)
                || self.candidate.roots.contains_key(&block)
                || self
                    .candidate
                    .bindings
                    .values()
                    .any(|value| *value == block)
                || !crate::state::is_live_block(&self.candidate.devices, block, record_seq)
                || !allocated_before(&self.candidate.devices, block, record_seq)
            {
                return Err(SuperblockError::InvalidBlockRef);
            }
            if let Some(id) = block.block.large_id() {
                large.entry(block.device).or_default().insert(id);
            } else if let Some((version, index)) = block.block.ordinary_fields() {
                ordinary
                    .entry((block.device, block.block.level(), version))
                    .or_default()
                    .insert(index);
            } else {
                return Err(SuperblockError::InvalidBlockRef);
            }
        }

        let mut next = Arc::clone(&self.candidate);
        let mut changes = Vec::new();
        for ((device, level, version), slots) in ordinary {
            let ranges = merge_ranges(slots)?;
            apply(
                Arc::make_mut(&mut next),
                &mut changes,
                SystemChange::ReleaseSegmentBlocks {
                    device,
                    level,
                    version,
                    ranges,
                },
                record_seq,
            )?;
            if segment_fully_released(&next.devices, device, level, version)? {
                if segment_is_active(&next.devices, device, level, version)? {
                    let next_version = SegmentVersion::new(
                        version
                            .raw()
                            .checked_add(1)
                            .ok_or(SuperblockError::InvalidBlockRef)?,
                    )
                    .map_err(|_| SuperblockError::InvalidBlockRef)?;
                    apply(
                        Arc::make_mut(&mut next),
                        &mut changes,
                        SystemChange::CreateActiveSegment {
                            device,
                            level,
                            version: next_version,
                        },
                        record_seq,
                    )?;
                }
                apply(
                    Arc::make_mut(&mut next),
                    &mut changes,
                    SystemChange::RemoveSegment {
                        device,
                        level,
                        version,
                    },
                    record_seq,
                )?;
            }
        }
        for (device, ids) in large {
            for id in ids {
                apply(
                    Arc::make_mut(&mut next),
                    &mut changes,
                    SystemChange::ReleaseLargeBlock { device, id },
                    record_seq,
                )?;
                apply(
                    Arc::make_mut(&mut next),
                    &mut changes,
                    SystemChange::RemoveLargeBlock { device, id },
                    record_seq,
                )?;
            }
        }
        self.candidate = next;
        self.changes.extend(changes);
        Ok(())
    }
}

fn apply<S: ApplicationState>(
    state: &mut SuperblockState<S>,
    changes: &mut Vec<TransactionChange<S::Delta>>,
    change: SystemChange,
    record_seq: pi_block_device::RecordSeq,
) -> Result<(), SuperblockError> {
    crate::change::apply_system_change(state, &change, record_seq)
        .map_err(|_| SuperblockError::InvalidBlockRef)?;
    changes.push(TransactionChange::System(change));
    Ok(())
}

fn merge_ranges(slots: BTreeSet<u32>) -> Result<Vec<BlockRange>, SuperblockError> {
    let mut ranges = Vec::new();
    let mut iter = slots.into_iter();
    let Some(mut start) = iter.next() else {
        return Ok(ranges);
    };
    let mut end = start
        .checked_add(1)
        .ok_or(SuperblockError::InvalidBlockRef)?;
    for slot in iter {
        if slot == end {
            end = end.checked_add(1).ok_or(SuperblockError::InvalidBlockRef)?;
        } else {
            ranges.push(range(start, end)?);
            start = slot;
            end = slot
                .checked_add(1)
                .ok_or(SuperblockError::InvalidBlockRef)?;
        }
    }
    ranges.push(range(start, end)?);
    Ok(ranges)
}

fn range(start: u32, end: u32) -> Result<BlockRange, SuperblockError> {
    Ok(BlockRange {
        start,
        len: NonZeroU32::new(end - start).ok_or(SuperblockError::InvalidBlockRef)?,
    })
}

fn segment(
    devices: &crate::DeviceTable,
    device: DeviceKey,
    level: u8,
    version: SegmentVersion,
) -> Result<
    (
        &pi_block_device::LevelVersions,
        &pi_block_device::SegmentData,
    ),
    SuperblockError,
> {
    let levels = &devices
        .get(device)
        .ok_or(SuperblockError::InvalidBlockRef)?
        .state()
        .levels;
    let versions = levels
        .get(level as usize)
        .ok_or(SuperblockError::InvalidBlockRef)?;
    let offset = version
        .raw()
        .checked_sub(versions.min_version.raw())
        .ok_or(SuperblockError::InvalidBlockRef)? as usize;
    let data = versions
        .versions
        .get(offset)
        .and_then(Option::as_ref)
        .ok_or(SuperblockError::InvalidBlockRef)?;
    Ok((versions, data))
}

fn segment_fully_released(
    devices: &crate::DeviceTable,
    device: DeviceKey,
    level: u8,
    version: SegmentVersion,
) -> Result<bool, SuperblockError> {
    Ok(segment(devices, device, level, version)?
        .1
        .released_at
        .iter()
        .all(|released| !released.is_null()))
}

fn segment_is_active(
    devices: &crate::DeviceTable,
    device: DeviceKey,
    level: u8,
    version: SegmentVersion,
) -> Result<bool, SuperblockError> {
    let (versions, _) = segment(devices, device, level, version)?;
    let offset = version.raw() - versions.min_version.raw();
    Ok(offset as usize + 1 == versions.versions.len())
}

fn allocated_before(
    devices: &crate::DeviceTable,
    block: BlockRef,
    record_seq: pi_block_device::RecordSeq,
) -> bool {
    let Some(slot) = devices.get(block.device) else {
        return false;
    };
    if let Some(id) = block.block.large_id() {
        return slot
            .state()
            .large_blocks
            .entries
            .get(&id)
            .is_some_and(|data| data.allocated_at < record_seq);
    }
    let Some((version, index)) = block.block.ordinary_fields() else {
        return false;
    };
    let Ok((_, data)) = segment(devices, block.device, block.block.level(), version) else {
        return false;
    };
    let mut end = 0_u32;
    data.allocation_runs.iter().any(|run| {
        let start = end;
        end = end.saturating_add(run.len);
        index >= start && index < end && run.allocated_at < record_seq
    })
}

pub(crate) fn retirements_between(
    old: &crate::DeviceTable,
    new: &crate::DeviceTable,
) -> Vec<Vec<RetiredObject>> {
    old.iter()
        .zip(new.iter())
        .map(|(old_slot, new_slot)| retired_on_device(old_slot.state(), new_slot.state()))
        .collect()
}

fn retired_on_device(old: &BlockDeviceState, new: &BlockDeviceState) -> Vec<RetiredObject> {
    let mut retired = Vec::new();
    for (level, versions) in old.levels.iter().enumerate() {
        for (offset, data) in versions.versions.iter().enumerate() {
            let Some(data) = data else {
                continue;
            };
            let raw = versions.min_version.raw() + offset as u32;
            let Ok(version) = SegmentVersion::new(raw) else {
                continue;
            };
            let present = raw
                .checked_sub(new.levels[level].min_version.raw())
                .and_then(|new_offset| new.levels[level].versions.get(new_offset as usize))
                .is_some_and(Option::is_some);
            if !present && !data.allocation_runs.is_empty() {
                retired.push(RetiredObject::Segment {
                    level: level as u8,
                    version,
                });
            }
        }
    }
    for id in old.large_blocks.entries.keys() {
        if !new.large_blocks.entries.contains_key(id) {
            retired.push(RetiredObject::LargeBlock { id: *id });
        }
    }
    retired
}
