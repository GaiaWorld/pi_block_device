use bytes::Bytes;
use futures_core::Stream;
use pi_block_device::{BlockDeviceBackend, BlockRef, DeviceKey};

use super::{StreamWriteError, Transaction};
use crate::{ApplicationCodec, ApplicationState, DeviceResolver};
use futures_lite::StreamExt;
use pi_block_device::{
    BlockDeviceWriteBatch, BlockWriteChunk, BlockWritePosition, DeviceOperation,
};

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    #[allow(clippy::manual_async_fn)]
    pub(super) fn write_stream_on_impl<E, St>(
        &mut self,
        _device: DeviceKey,
        _logical_len: u64,
        _stream: St,
    ) -> impl Future<Output = Result<BlockRef, StreamWriteError<E>>> + Send + '_
    where
        E: Send + Sync + 'static,
        St: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    {
        async move {
            let allocation = self
                .alloc_on_impl(_device, _logical_len)
                .map_err(StreamWriteError::Superblock)?;
            let block = allocation.block;
            let index = self
                .device_index(_device)
                .map_err(StreamWriteError::Superblock)?;
            let record = self.allocations.len() - 1;
            let writer = self.writer_guard.take();
            let mut batches = std::mem::take(&mut self.batches);
            let mut stream = _stream;
            let mut written = 0_u64;
            while let Some(item) = stream.next().await {
                let chunk = item.map_err(StreamWriteError::Item)?;
                if chunk.is_empty() {
                    continue;
                }
                let end = written
                    .checked_add(chunk.len() as u64)
                    .filter(|end| *end <= _logical_len)
                    .ok_or(StreamWriteError::StreamTooLong {
                        written,
                        logical_len: _logical_len,
                    })?;
                let position = BlockWritePosition::new(written, _logical_len).map_err(|_| {
                    StreamWriteError::Superblock(crate::SuperblockError::InvalidBlockRef)
                })?;
                let request = BlockWriteChunk::new(block.block, chunk, position).map_err(|_| {
                    StreamWriteError::Superblock(crate::SuperblockError::InvalidBlockRef)
                })?;
                if batches[index].is_none() {
                    batches[index] = Some(self.devices[index].begin_write_batch().map_err(
                        |mut source| {
                            source.operation = DeviceOperation::BeginWriteBatch;
                            source.device_key = Some(_device);
                            StreamWriteError::Superblock(crate::SuperblockError::Device { source })
                        },
                    )?);
                }
                let batch = batches[index].as_mut().ok_or(StreamWriteError::Superblock(
                    crate::SuperblockError::InvalidState,
                ))?;
                batch.write(request).await.map_err(|mut source| {
                    source.operation = DeviceOperation::Write;
                    source.device_key = Some(_device);
                    StreamWriteError::Superblock(crate::SuperblockError::Device { source })
                })?;
                written = end;
            }
            if written != _logical_len {
                return Err(StreamWriteError::StreamTooShort {
                    written,
                    logical_len: _logical_len,
                });
            }
            self.allocations[record].written = true;
            self.batches = batches;
            self.writer_guard = writer;
            Ok(block)
        }
    }
}
