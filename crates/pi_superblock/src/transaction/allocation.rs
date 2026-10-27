use std::num::NonZeroU64;

use pi_block_device::{BlockId, BlockRef, DeviceKey, SegmentVersion};

use super::{AllocatedBlock, AllocationRecord, Transaction};
use crate::{ApplicationCodec, ApplicationState, DeviceResolver, SuperblockError, SystemChange};

const SEGMENT_BYTES: u64 = 256 * 1024 * 1024;

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub(super) fn alloc_on_impl(
        &mut self,
        device: DeviceKey,
        len: u64,
    ) -> Result<AllocatedBlock, SuperblockError> {
        self.ensure_open()?;
        let index = self.device_index(device)?;
        let logical_len = NonZeroU64::new(len).ok_or(SuperblockError::InvalidBlockRef)?;
        let level = self.devices[index]
            .capabilities()
            .level_sizes
            .iter()
            .position(|size| len <= u64::from(*size));
        let block = match level {
            Some(level) => self.allocate_ordinary(device, level as u8)?,
            None => self.allocate_large(device, len)?,
        };
        self.allocations.push(AllocationRecord {
            block,
            written: false,
        });
        Ok(AllocatedBlock { block, logical_len })
    }

    fn allocate_large(&mut self, device: DeviceKey, len: u64) -> Result<BlockRef, SuperblockError> {
        let id = self
            .candidate
            .devices
            .get(device)
            .ok_or(SuperblockError::InvalidBlockRef)?
            .state()
            .large_blocks
            .next_id
            .next_id()
            .map_err(|_| SuperblockError::InvalidBlockRef)?
            .0;
        self.system(SystemChange::AllocateLargeBlock {
            device,
            id,
            logical_len: len,
        })
        .map_err(|_| SuperblockError::InvalidBlockRef)?;
        Ok(BlockRef {
            device,
            block: BlockId::large(id),
        })
    }

    fn allocate_ordinary(
        &mut self,
        device: DeviceKey,
        level: u8,
    ) -> Result<BlockRef, SuperblockError> {
        let index = self.device_index(device)?;
        let slot_size = u64::from(self.devices[index].capabilities().level_sizes[level as usize]);
        let capacity = u32::try_from(SEGMENT_BYTES / slot_size)
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        let (mut version, mut index) = self.active_position(device, level)?;
        if index == capacity {
            version = SegmentVersion::new(
                version
                    .raw()
                    .checked_add(1)
                    .ok_or(SuperblockError::InvalidBlockRef)?,
            )
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
            self.system(SystemChange::CreateActiveSegment {
                device,
                level,
                version,
            })
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
            index = 0;
        }
        if index >= capacity {
            return Err(SuperblockError::InvalidBlockRef);
        }
        self.system(SystemChange::AllocateSegmentBlocks {
            device,
            level,
            version,
            len: 1,
        })
        .map_err(|_| SuperblockError::InvalidBlockRef)?;
        let block = BlockId::ordinary(level, version, index)
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        Ok(BlockRef { device, block })
    }

    fn active_position(
        &self,
        device: DeviceKey,
        level: u8,
    ) -> Result<(SegmentVersion, u32), SuperblockError> {
        let state = self
            .candidate
            .devices
            .get(device)
            .ok_or(SuperblockError::InvalidBlockRef)?;
        let level = state
            .state()
            .levels
            .get(level as usize)
            .ok_or(SuperblockError::InvalidBlockRef)?;
        let offset = level
            .versions
            .len()
            .checked_sub(1)
            .ok_or(SuperblockError::InvalidBlockRef)?;
        let segment = level
            .versions
            .get(offset)
            .and_then(Option::as_ref)
            .ok_or(SuperblockError::InvalidBlockRef)?;
        let version = u32::try_from(offset)
            .ok()
            .and_then(|offset| level.min_version.raw().checked_add(offset))
            .and_then(|raw| SegmentVersion::new(raw).ok())
            .ok_or(SuperblockError::InvalidBlockRef)?;
        let index = u32::try_from(segment.released_at.len())
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
        Ok((version, index))
    }
}
