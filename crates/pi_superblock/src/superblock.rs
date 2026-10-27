mod implementation;

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use pi_block_device::{BlockDeviceBackend, BlockRef, BoxError, DeviceKey};

use crate::append_log::SuperblockAppendLog;
#[cfg(feature = "test-support")]
use crate::append_log::{MaintenanceError, MaintenanceOutcome};
use crate::transaction::Transaction;
use crate::{
    ApplicationCodec, ApplicationState, CreateError, DeviceLabel, DeviceRegistration, OpenError,
    SuperblockError, SuperblockSnapshot, SuperblockState, SuperblockStatus,
};
use arc_swap::ArcSwap;
use smallvec::SmallVec;

#[cfg(test)]
mod retirement_tests;
mod support;

// Inline capacity only; larger device sets spill to the heap.
const INLINE_DEVICES: usize = 4;

/// A backend and its initial human-readable registration label.
pub struct InitialDevice<D: BlockDeviceBackend> {
    pub label: DeviceLabel,
    pub device: Arc<D>,
}

/// Resolves persisted device registrations during `open`.
pub trait DeviceResolver<D: BlockDeviceBackend>: Send + Sync + 'static {
    fn resolve<'a>(
        &'a self,
        key: DeviceKey,
        registration: &'a DeviceRegistration,
    ) -> impl Future<Output = Result<Arc<D>, ResolveError>> + Send + 'a;
}

#[derive(Debug)]
/// Structured failure returned by a device resolver.
pub struct ResolveError {
    pub device_key: DeviceKey,
    pub source: BoxError,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "device {} resolution failed: {}",
            self.device_key.raw(),
            self.source
        )
    }
}

impl Error for ResolveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Configuration for the append log used by a superblock.
/// `path` is the sole append-log root; no marker or derived path is created.
pub struct SuperblockLogConfig {
    /// Append-log root path owned by `pi_append_log`.
    pub path: PathBuf,
    /// Threshold retained for future rotation support.
    pub rotate_threshold: u64,
    /// Dependency append options (called AppendLogOptions in the design).
    /// Every checkpoint and delta forces durable=true, even when configured false.
    pub append_log_options: pi_append_log::AppendOptions,
}

#[allow(dead_code)]
pub struct Superblock<S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    log: Arc<SuperblockAppendLog>,
    resolver: Arc<R>,
    codec: Arc<C>,
    devices: SmallVec<[Arc<D>; INLINE_DEVICES]>,
    published: ArcSwap<SuperblockState<S>>,
    // Serialize publication with physical lease acquisition, not Arc loading alone.
    publication_access: Mutex<()>,
    status: Arc<crate::state::AtomicStatus>,
    writer_gate: async_lock::Mutex<()>,
}

impl<S, C, D, R> fmt::Debug for Superblock<S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.debug_impl(formatter)
    }
}

impl<S, C, D, R> Superblock<S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    #[allow(clippy::manual_async_fn)]
    /// Creates and durably persists the initial checkpoint.
    /// Validates system state and codec output; APP reference visitor validation is not provided.
    /// The returned future is `Send + 'static`; create never resolves devices.
    pub fn create(
        log: SuperblockLogConfig,
        devices: Vec<InitialDevice<D>>,
        resolver: Arc<R>,
        codec: Arc<C>,
        initial_state: S,
    ) -> impl Future<Output = Result<Self, CreateError>> + Send + 'static {
        Self::create_impl(log, devices, resolver, codec, initial_state)
    }

    #[allow(clippy::manual_async_fn)]
    /// Recovers, resolves, validates, and reconciles all registered devices.
    /// Publishes only after every registered device has passed recovery.
    /// The returned future is `Send + 'static`.
    pub fn open(
        log: SuperblockLogConfig,
        resolver: Arc<R>,
        codec: Arc<C>,
    ) -> impl Future<Output = Result<Self, OpenError>> + Send + 'static {
        Self::open_impl(log, resolver, codec)
    }

    /// Returns the current superblock status.
    pub fn status(&self) -> SuperblockStatus {
        self.status_impl()
    }

    /// Opens an all-or-nothing fixed snapshot of the currently published state.
    /// Persistent collections do not replace the acquired physical leases.
    pub fn open_snapshot(&self) -> Result<SuperblockSnapshot<S, D>, SuperblockError> {
        self.open_snapshot_impl()
    }

    /// Reads a block against the currently published state using a temporary lease.
    #[allow(clippy::manual_async_fn)]
    pub fn read(
        &self,
        block: BlockRef,
    ) -> impl Future<Output = Result<D::ReadBuf, SuperblockError>> + Send + '_ {
        self.read_impl(block)
    }

    #[allow(clippy::manual_async_fn)]
    pub fn begin_write(
        &self,
    ) -> impl Future<Output = Result<Transaction<'_, S, C, D, R>, SuperblockError>> + Send + '_
    {
        self.begin_write_impl()
    }
}

impl<S, C, D, R> Superblock<S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub async fn run_log_maintenance(&self) -> Result<MaintenanceOutcome<S>, MaintenanceError> {
        self.run_log_maintenance_internal().await
    }
}

impl<S, C, D, R> Superblock<S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub async fn run_pending_maintenance(&self) -> Result<MaintenanceOutcome<S>, MaintenanceError> {
        self.run_pending_maintenance_internal().await
    }
}
