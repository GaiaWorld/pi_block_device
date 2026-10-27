use std::collections::{BTreeMap, VecDeque};
use std::future::{Future, ready};
use std::io;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use pi_block_device::{
    BackendCapabilities, BlockDeviceBackend, BlockDeviceId, BlockDeviceState,
    BlockDeviceWriteBatch, BlockId, BlockWriteChunk, BoxError, DeviceError, DeviceKey,
    DeviceOperation, InspectReport, ProtectedReadBuf, ReconcileError, ReconcileFailureKind,
    ReconcileProgress, ReconcileReport, RetiredObject, RetirementReport,
};
use pi_superblock::{DeviceRegistration, DeviceResolver, ResolveError};

#[derive(Clone, Copy, Debug)]
pub enum InspectMode {
    Empty,
    NonEmpty,
    Failure,
}

#[derive(Clone, Copy, Debug)]
pub enum CapabilityVariant {
    Protocol,
    Base,
    Level,
    Large,
    Optional,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ResolverStage {
    Resolve,
}

#[derive(Default)]
pub struct BackendCalls {
    pub inspect: usize,
    pub begin_write: usize,
    pub acquire_snapshot_lease: usize,
    pub read: usize,
    pub reconcile: usize,
    pub enqueue_retirements: usize,
    pub reconcile_attempts: Vec<ReconcileAttempt>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconcileAttempt {
    pub source: Option<String>,
    pub progress: ReconcileProgress,
    pub state: Arc<BlockDeviceState>,
}

#[derive(Clone)]
pub struct MockBackend {
    pub id: BlockDeviceId,
    pub capabilities: BackendCapabilities,
    pub inspect_mode: InspectMode,
    pub calls: Arc<Mutex<BackendCalls>>,
    reconcile_failures: Arc<Mutex<VecDeque<ReconcileFailure>>>,
    reconcile_states: Arc<Mutex<Vec<Arc<BlockDeviceState>>>>,
}

type ReconcileFailure = (ReconcileFailureKind, ReconcileProgress, Option<String>);

impl MockBackend {
    pub fn fixed(id: BlockDeviceId) -> Self {
        Self {
            id,
            capabilities: BackendCapabilities::fixed(),
            inspect_mode: InspectMode::Empty,
            calls: Arc::new(Mutex::new(BackendCalls::default())),
            reconcile_failures: Arc::new(Mutex::new(VecDeque::new())),
            reconcile_states: Arc::new(Mutex::new(Vec::new())),
        }
    }
    pub fn non_empty(mut self) -> Self {
        self.inspect_mode = InspectMode::NonEmpty;
        self
    }
    pub fn with_capabilities(mut self, capabilities: BackendCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
    pub fn incompatible_variant(mut self, variant: CapabilityVariant) -> Self {
        match variant {
            CapabilityVariant::Protocol => self.capabilities.protocol_version = 2,
            CapabilityVariant::Base => self.capabilities.base_block_size += 1,
            CapabilityVariant::Level => self.capabilities.level_sizes[0] += 1,
            CapabilityVariant::Large => self.capabilities.independent_large_blocks = false,
            CapabilityVariant::Optional => self.capabilities.optional_flags = 1,
        }
        self
    }
    pub fn incompatible(mut self) -> Self {
        self.capabilities.protocol_version = 2;
        self
    }
    pub fn reconcile_failures(self, kinds: impl IntoIterator<Item = ReconcileFailureKind>) -> Self {
        self.reconcile_failures
            .lock()
            .expect("failures")
            .extend(kinds.into_iter().map(|kind| {
                (
                    kind,
                    ReconcileProgress {
                        planned_actions: 1,
                        completed_actions: 0,
                        remaining_actions: 1,
                        partial_progress: false,
                        media_metadata_synced: false,
                    },
                    Some("mock reconcile failure".to_owned()),
                )
            }));
        self
    }
    pub fn reconcile_failure_queue(
        self,
        failures: impl IntoIterator<Item = (ReconcileFailureKind, ReconcileProgress, Option<String>)>,
    ) -> Self {
        self.reconcile_failures
            .lock()
            .expect("failures")
            .extend(failures);
        self
    }
    pub fn reconcile_states(&self) -> Vec<Arc<BlockDeviceState>> {
        self.reconcile_states.lock().expect("states").clone()
    }
}

#[derive(Default)]
pub struct MockResolver {
    pub calls: Arc<Mutex<Vec<DeviceKey>>>,
    pub devices: BTreeMap<DeviceKey, Arc<MockBackend>>,
    pub failures: BTreeMap<(DeviceKey, ResolverStage), String>,
}
// Extended scenario configuration is intentionally kept in this test-only mock.
impl MockResolver {
    pub fn with(key: DeviceKey, backend: Arc<MockBackend>) -> Self {
        let mut devices = BTreeMap::new();
        devices.insert(key, backend);
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            devices,
            failures: BTreeMap::new(),
        }
    }
    pub fn fail_at(
        mut self,
        key: DeviceKey,
        stage: ResolverStage,
        message: impl Into<String>,
    ) -> Self {
        self.failures.insert((key, stage), message.into());
        self
    }
}
impl DeviceResolver<MockBackend> for MockResolver {
    fn resolve<'a>(
        &'a self,
        key: DeviceKey,
        _registration: &'a DeviceRegistration,
    ) -> impl Future<Output = Result<Arc<MockBackend>, ResolveError>> + Send + 'a {
        self.calls.lock().expect("resolver calls").push(key);
        ready(
            self.failures
                .get(&(key, ResolverStage::Resolve))
                .map_or_else(
                    || {
                        self.devices.get(&key).cloned().ok_or_else(|| ResolveError {
                            device_key: key,
                            source: Box::new(io::Error::new(
                                io::ErrorKind::NotFound,
                                "unknown device key",
                            )),
                        })
                    },
                    |message| {
                        Err(ResolveError {
                            device_key: key,
                            source: Box::new(io::Error::other(message.clone())),
                        })
                    },
                ),
        )
    }
}

#[derive(Clone, Copy)]
pub struct MockLease;
pub struct MockReadBuf {
    bytes: Bytes,
    lease: MockLease,
}
impl AsRef<[u8]> for MockReadBuf {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl ProtectedReadBuf for MockReadBuf {
    type Lease = MockLease;
    fn lease(&self) -> &Self::Lease {
        &self.lease
    }
}
pub struct MockBatch;
fn device_error(operation: DeviceOperation) -> DeviceError {
    DeviceError {
        operation,
        device_key: None,
        source: Box::new(io::Error::other("mock backend operation unavailable")),
    }
}
impl BlockDeviceWriteBatch for MockBatch {
    async fn write(&mut self, _: BlockWriteChunk) -> Result<(), DeviceError> {
        Err(device_error(DeviceOperation::Write))
    }
    async fn prepare(self) -> Result<(), DeviceError> {
        Err(device_error(DeviceOperation::Prepare))
    }
}
impl BlockDeviceBackend for MockBackend {
    type SnapshotLease = MockLease;
    type ReadBuf = MockReadBuf;
    type WriteBatch = MockBatch;
    fn device_id(&self) -> BlockDeviceId {
        self.id
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.capabilities.clone()
    }
    fn inspect(&self) -> impl Future<Output = Result<InspectReport, DeviceError>> + Send + '_ {
        self.calls.lock().expect("calls").inspect += 1;
        async move {
            Ok(match self.inspect_mode {
                InspectMode::Failure => return Err(device_error(DeviceOperation::Inspect)),
                InspectMode::Empty => InspectReport {
                    empty: true,
                    observed_objects: 0,
                },
                InspectMode::NonEmpty => InspectReport {
                    empty: false,
                    observed_objects: 1,
                },
            })
        }
    }
    fn begin_write_batch(self: &Arc<Self>) -> Result<Self::WriteBatch, DeviceError> {
        self.calls.lock().expect("calls").begin_write += 1;
        Err(device_error(DeviceOperation::BeginWriteBatch))
    }
    fn acquire_snapshot_lease(
        self: &Arc<Self>,
        _: Arc<BlockDeviceState>,
    ) -> Result<Self::SnapshotLease, DeviceError> {
        self.calls.lock().expect("calls").acquire_snapshot_lease += 1;
        Err(device_error(DeviceOperation::AcquireSnapshotLease))
    }
    fn read<'a>(
        self: &'a Arc<Self>,
        _: &'a Self::SnapshotLease,
        _: BlockId,
    ) -> impl Future<Output = Result<Self::ReadBuf, DeviceError>> + Send + 'a {
        self.calls.lock().expect("calls").read += 1;
        async { Err(device_error(DeviceOperation::Read)) }
    }
    fn reconcile(
        self: &Arc<Self>,
        expected: Arc<BlockDeviceState>,
    ) -> impl Future<Output = Result<ReconcileReport, ReconcileError>> + Send + '_ {
        self.calls.lock().expect("calls").reconcile += 1;
        self.reconcile_states
            .lock()
            .expect("states")
            .push(expected.clone());
        let failure = self
            .reconcile_failures
            .lock()
            .expect("failures")
            .pop_front();
        async move {
            let (failure, progress, source) = failure.unwrap_or((
                ReconcileFailureKind::RepairIo,
                ReconcileProgress {
                    planned_actions: 0,
                    completed_actions: 0,
                    remaining_actions: 0,
                    partial_progress: false,
                    media_metadata_synced: true,
                },
                None,
            ));
            self.calls
                .lock()
                .expect("calls")
                .reconcile_attempts
                .push(ReconcileAttempt {
                    source: source.clone(),
                    progress,
                    state: expected.clone(),
                });
            if progress.planned_actions == 0 {
                Ok(ReconcileReport {
                    inspected_objects: 0,
                    progress,
                })
            } else {
                Err(ReconcileError {
                    kind: failure,
                    progress,
                    source: source.map(|value| Box::new(io::Error::other(value)) as BoxError),
                })
            }
        }
    }
    async fn reclaim_retirements(self: &Arc<Self>) -> Result<RetirementReport, DeviceError> {
        Err(device_error(DeviceOperation::ReclaimRetirements))
    }
    fn enqueue_retirements(self: &Arc<Self>, _: Vec<RetiredObject>) -> Result<(), DeviceError> {
        self.calls.lock().expect("calls").enqueue_retirements += 1;
        Err(device_error(DeviceOperation::EnqueueRetirements))
    }
}
