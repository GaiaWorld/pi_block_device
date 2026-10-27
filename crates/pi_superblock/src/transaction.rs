use std::num::NonZeroU64;

use bytes::Bytes;
use futures_core::Stream;
use pi_block_device::{BlockRef, DeviceKey};

use crate::append_log::SuperblockAppendLog;
use crate::{
    ApplicationApplyContext, ApplicationCodec, ApplicationState, BindingName, DeviceResolver,
    SuperblockError, SuperblockState, SystemChange, TransactionChange,
};
use std::sync::{Arc, Mutex};

mod allocation;
mod implementation;
pub(crate) mod release;
mod stream;
mod write;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommitResult {
    pub record_seq: pi_block_device::RecordSeq,
    pub status: crate::SuperblockStatus,
}

impl CommitResult {
    pub fn record_seq(&self) -> pi_block_device::RecordSeq {
        self.record_seq
    }

    pub fn status(&self) -> crate::SuperblockStatus {
        self.status
    }
}

pub struct AllocatedBlock {
    block: BlockRef,
    logical_len: NonZeroU64,
}
pub struct BlockWrite {
    allocation: AllocatedBlock,
    data: Bytes,
}
#[derive(Debug)]
pub enum StreamWriteError<E> {
    Item(E),
    StreamTooShort { written: u64, logical_len: u64 },
    StreamTooLong { written: u64, logical_len: u64 },
    Superblock(SuperblockError),
}
impl BlockWrite {
    pub fn new(allocation: AllocatedBlock, data: Bytes) -> Result<Self, SuperblockError> {
        implementation::block_write(allocation, data)
    }
}
/// Borrows concrete runtime fields; does not own or borrow a whole Superblock.
/// A missing writer guard denotes a closed transaction; no cross-transaction identity is provided.
pub struct Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub(crate) candidate: Arc<SuperblockState<S>>,
    /// Fixed logical baseline only; this Arc does not provide physical protection.
    pub(crate) published_at_begin: Arc<SuperblockState<S>>,
    pub(crate) changes: Vec<TransactionChange<S::Delta>>,
    pub(crate) _codec: &'a C,
    pub(crate) devices: &'a [Arc<D>],
    pub(crate) batches: Vec<Option<D::WriteBatch>>,
    // R remains in the public generic shape; no resolver resource is used by a transaction.
    pub(crate) _marker: std::marker::PhantomData<R>,
    pub(crate) published: &'a arc_swap::ArcSwap<SuperblockState<S>>,
    pub(crate) publication_access: &'a Mutex<()>,
    pub(crate) log: &'a SuperblockAppendLog,
    pub(crate) status: &'a crate::state::AtomicStatus,
    // Option allows commit(&mut self) to release the borrowed guard before Drop.
    pub(crate) writer_guard: Option<async_lock::MutexGuard<'a, ()>>,
    pub(crate) allocations: Vec<AllocationRecord>,
}
pub(crate) struct AllocationRecord {
    block: BlockRef,
    written: bool,
}
impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub fn application(&self) -> &S {
        self.application_impl()
    }

    /// Looks up a binding in the current candidate, including local changes.
    /// The result may refer to a new block that transaction reads cannot read yet.
    pub fn get_binding(&self, _name: &BindingName) -> Option<BlockRef> {
        self.get_binding_impl(_name)
    }

    /// Reads a block committed in the transaction baseline and still live in
    /// the current candidate. New allocations and released blocks are rejected
    /// with InvalidBlockRef. RecoveryRequired must fence reads; backend failures
    /// retain their source. No transaction-wide baseline leases are provided.
    /// A temporary lease protects this read, not the whole logical baseline.
    /// The logical baseline Arc is not a lease. Returned buffers must retain
    /// their own protection. The helper borrows fields, never the whole transaction.
    pub fn read(
        &self,
        _block: BlockRef,
    ) -> impl std::future::Future<Output = Result<D::ReadBuf, SuperblockError>> + Send + '_ {
        implementation::read::<S, D>(
            &self.published_at_begin,
            &self.candidate,
            self.devices,
            self.status,
            self.writer_guard.is_some(),
            _block,
        )
    }
    #[allow(clippy::manual_async_fn)]
    pub fn commit(
        &mut self,
    ) -> impl std::future::Future<Output = Result<CommitResult, SuperblockError>> + Send + '_ {
        self.commit_impl()
    }
    pub fn apply_application(&mut self, delta: S::Delta) -> Result<(), SuperblockError> {
        self.apply_application_impl(delta)
    }

    pub fn add_root(&mut self, block: BlockRef) -> Result<(), SuperblockError> {
        self.add_root_impl(block)
    }
    pub fn remove_root(&mut self, block: BlockRef) -> Result<(), SuperblockError> {
        self.remove_root_impl(block)
    }
    pub fn bind(&mut self, name: BindingName, block: BlockRef) -> Result<(), SuperblockError> {
        self.bind_impl(name, block)
    }
    pub fn unbind(&mut self, name: &BindingName) -> Result<(), SuperblockError> {
        self.unbind_impl(name)
    }
}

impl<'a, S, C, D, R> Drop for Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    fn drop(&mut self) {
        self.cleanup()
    }
}

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub fn alloc_on(
        &mut self,
        device: DeviceKey,
        len: u64,
    ) -> Result<AllocatedBlock, SuperblockError> {
        self.alloc_on_impl(device, len)
    }
}

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub fn release(&mut self, blocks: Vec<BlockRef>) -> Result<(), SuperblockError> {
        self.release_impl(blocks)
    }
}

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub fn write_on(
        &mut self,
        _device: DeviceKey,
        _data: Bytes,
    ) -> impl Future<Output = Result<BlockRef, SuperblockError>> + Send + '_ {
        self.write_on_impl(_device, _data)
    }
}

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub fn write_batch(
        &mut self,
        _writes: Vec<BlockWrite>,
    ) -> impl Future<Output = Result<(), SuperblockError>> + Send + '_ {
        self.write_batch_impl(_writes)
    }
}

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub fn write_stream_on<E, St>(
        &mut self,
        _device: DeviceKey,
        _logical_len: u64,
        _stream: St,
    ) -> impl Future<Output = Result<BlockRef, StreamWriteError<E>>> + Send + '_
    where
        E: Send + Sync + 'static,
        St: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    {
        self.write_stream_on_impl(_device, _logical_len, _stream)
    }
}
