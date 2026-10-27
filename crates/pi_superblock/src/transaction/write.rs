use bytes::Bytes;
use pi_block_device::{
    BlockDeviceBackend, BlockDeviceWriteBatch, BlockRef, BlockWriteChunk, BlockWritePosition,
    DeviceKey, DeviceOperation,
};

use super::{BlockWrite, Transaction};
use crate::{ApplicationCodec, ApplicationState, DeviceResolver, SuperblockError};

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    fn checked_allocation(&self, write: &BlockWrite) -> Result<usize, SuperblockError> {
        let index = self
            .allocations
            .iter()
            .position(|record| record.block == write.allocation.block)
            .ok_or(SuperblockError::InvalidBlockRef)?;
        if self.allocations[index].written
            || write.data.len() as u64 != write.allocation.logical_len.get()
            || !crate::state::is_live_block(
                &self.candidate.devices,
                write.allocation.block,
                self.candidate_seq()?,
            )
        {
            return Err(SuperblockError::InvalidBlockRef);
        }
        // Records are local bookkeeping, not proof of cross-transaction credential identity.
        Ok(index)
    }

    #[allow(clippy::manual_async_fn)]
    pub(super) fn write_on_impl(
        &mut self,
        _device: DeviceKey,
        _data: Bytes,
    ) -> impl Future<Output = Result<BlockRef, SuperblockError>> + Send + '_ {
        async move {
            let allocation = self.alloc_on_impl(_device, _data.len() as u64)?;
            let block = allocation.block;
            self.write_batch_impl(vec![BlockWrite::new(allocation, _data)?])
                .await?;
            Ok(block)
        }
    }

    #[allow(clippy::manual_async_fn)]
    pub(super) fn write_batch_impl(
        &mut self,
        _writes: Vec<BlockWrite>,
    ) -> impl Future<Output = Result<(), SuperblockError>> + Send + '_ {
        async move {
            self.ensure_open()?;
            let mut requests = Vec::with_capacity(_writes.len());
            for write in _writes {
                let record = self.checked_allocation(&write)?;
                let device = self.device_index(write.allocation.block.device)?;
                if requests.iter().any(|(previous, _, _)| *previous == record) {
                    return Err(SuperblockError::InvalidBlockRef);
                }
                let position = BlockWritePosition::new(0, write.allocation.logical_len.get())
                    .map_err(|_| SuperblockError::InvalidBlockRef)?;
                let chunk =
                    BlockWriteChunk::new(write.allocation.block.block, write.data, position)
                        .map_err(|_| SuperblockError::InvalidBlockRef)?;
                requests.push((record, device, chunk));
            }
            // Move resources into the future scope. Errors and cancellation drop them, not data.
            let writer = self.writer_guard.take();
            let mut batches = std::mem::take(&mut self.batches);
            for (record, index, chunk) in requests {
                let key = self.allocations[record].block.device;
                if batches[index].is_none() {
                    batches[index] = Some(self.devices[index].begin_write_batch().map_err(
                        |mut source| {
                            source.operation = DeviceOperation::BeginWriteBatch;
                            source.device_key = Some(key);
                            SuperblockError::Device { source }
                        },
                    )?);
                }
                let batch = batches[index]
                    .as_mut()
                    .ok_or(SuperblockError::InvalidState)?;
                batch.write(chunk).await.map_err(|mut source| {
                    source.operation = DeviceOperation::Write;
                    source.device_key = Some(key);
                    SuperblockError::Device { source }
                })?;
                self.allocations[record].written = true;
            }
            self.batches = batches;
            self.writer_guard = writer;
            Ok(())
        }
    }
}
