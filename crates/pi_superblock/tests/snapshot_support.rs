use std::collections::BTreeMap;
use std::future::{Future, ready};
use std::io;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use pi_block_device::{
    BackendCapabilities, BlockDeviceBackend, BlockDeviceId, BlockDeviceState,
    BlockDeviceWriteBatch, BlockId, BlockWriteChunk, DeviceError, DeviceKey, DeviceOperation,
    InspectReport, ProtectedReadBuf, ReconcileError, ReconcileProgress, ReconcileReport,
    RetiredObject, RetirementReport,
};
use pi_superblock::{
    ApplicationApplyContext, ApplicationApplyError, ApplicationCodec, ApplicationCodecError,
    ApplicationEncoder, ApplicationState, DeviceLabel, DeviceRegistration, DeviceResolver,
    InitialDevice, ResolveError, Superblock, SuperblockLogConfig,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State(pub u64);
pub struct Delta(pub u64);
impl ApplicationState for State {
    type Delta = Delta;
    fn apply(
        &self,
        delta: &Delta,
        _: &ApplicationApplyContext<'_>,
    ) -> Result<Self, ApplicationApplyError> {
        Ok(Self(delta.0))
    }
}
pub struct Codec;
impl ApplicationCodec<State> for Codec {
    fn encode_state(
        &self,
        state: &State,
        out: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        out.put_u64_le(state.0)
    }
    fn decode_state(&self, bytes: Bytes) -> Result<State, ApplicationCodecError> {
        Ok(State(u64::from_le_bytes(
            bytes[..8].try_into().expect("state"),
        )))
    }
    fn encode_delta(
        &self,
        delta: &Delta,
        out: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        out.put_u64_le(delta.0)
    }
    fn decode_delta(&self, bytes: Bytes) -> Result<Delta, ApplicationCodecError> {
        Ok(Delta(u64::from_le_bytes(
            bytes[..8].try_into().expect("delta"),
        )))
    }
}

#[derive(Default)]
pub struct Calls {
    pub events: Vec<(DeviceKey, &'static str)>,
    pub reads: usize,
    pub drops: usize,
}
pub struct Lease {
    calls: Arc<Mutex<Calls>>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.calls.lock().expect("calls").drops += 1;
    }
}
pub struct ReadBuf {
    lease: Lease,
    bytes: Bytes,
}
impl AsRef<[u8]> for ReadBuf {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl ProtectedReadBuf for ReadBuf {
    type Lease = Lease;
    fn lease(&self) -> &Lease {
        &self.lease
    }
}
pub struct Batch;
impl BlockDeviceWriteBatch for Batch {
    async fn write(&mut self, _: BlockWriteChunk) -> Result<(), DeviceError> {
        Ok(())
    }
    async fn prepare(self) -> Result<(), DeviceError> {
        Ok(())
    }
}
pub struct Backend {
    key: DeviceKey,
    id: BlockDeviceId,
    pub calls: Arc<Mutex<Calls>>,
    fail_lease: bool,
}
impl Backend {
    pub fn new(raw: u32, fail_lease: bool) -> Self {
        let key = DeviceKey::new(raw).expect("key");
        Self {
            key,
            id: BlockDeviceId::from_bytes([raw as u8; 16]).expect("id"),
            calls: Arc::new(Mutex::new(Calls::default())),
            fail_lease,
        }
    }
}
fn error(operation: DeviceOperation, key: DeviceKey) -> DeviceError {
    DeviceError {
        operation,
        device_key: Some(key),
        source: Box::new(io::Error::other("injected")),
    }
}
impl BlockDeviceBackend for Backend {
    type SnapshotLease = Lease;
    type ReadBuf = ReadBuf;
    type WriteBatch = Batch;
    fn device_id(&self) -> BlockDeviceId {
        self.id
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::fixed()
    }
    fn inspect(&self) -> impl Future<Output = Result<InspectReport, DeviceError>> + Send + '_ {
        ready(Ok(InspectReport {
            empty: true,
            observed_objects: 0,
        }))
    }
    fn begin_write_batch(self: &Arc<Self>) -> Result<Batch, DeviceError> {
        Ok(Batch)
    }
    fn acquire_snapshot_lease(
        self: &Arc<Self>,
        _: Arc<BlockDeviceState>,
    ) -> Result<Lease, DeviceError> {
        self.calls
            .lock()
            .expect("calls")
            .events
            .push((self.key, "lease"));
        if self.fail_lease {
            Err(error(DeviceOperation::AcquireSnapshotLease, self.key))
        } else {
            Ok(Lease {
                calls: self.calls.clone(),
            })
        }
    }
    fn read<'a>(
        self: &'a Arc<Self>,
        lease: &'a Lease,
        _: BlockId,
    ) -> impl Future<Output = Result<ReadBuf, DeviceError>> + Send + 'a {
        self.calls.lock().expect("calls").reads += 1;
        ready(Ok(ReadBuf {
            lease: Lease {
                calls: lease.calls.clone(),
            },
            bytes: Bytes::from_static(b"data"),
        }))
    }
    fn reconcile(
        self: &Arc<Self>,
        _: Arc<BlockDeviceState>,
    ) -> impl Future<Output = Result<ReconcileReport, ReconcileError>> + Send + '_ {
        ready(Ok(ReconcileReport {
            inspected_objects: 0,
            progress: ReconcileProgress {
                planned_actions: 0,
                completed_actions: 0,
                remaining_actions: 0,
                partial_progress: false,
                media_metadata_synced: true,
            },
        }))
    }
    async fn reclaim_retirements(self: &Arc<Self>) -> Result<RetirementReport, DeviceError> {
        Ok(RetirementReport {
            attempted: 0,
            completed: 0,
            guarded: 0,
            retryable: 0,
        })
    }
    fn enqueue_retirements(self: &Arc<Self>, _: Vec<RetiredObject>) -> Result<(), DeviceError> {
        Ok(())
    }
}
#[derive(Default)]
pub struct Resolver {
    devices: BTreeMap<DeviceKey, Arc<Backend>>,
}
impl DeviceResolver<Backend> for Resolver {
    fn resolve<'a>(
        &'a self,
        key: DeviceKey,
        _: &'a DeviceRegistration,
    ) -> impl Future<Output = Result<Arc<Backend>, ResolveError>> + Send + 'a {
        ready(Ok(self.devices.get(&key).expect("device").clone()))
    }
}
pub async fn create(
    path: &std::path::Path,
    devices: &[Arc<Backend>],
) -> Superblock<State, Codec, Backend, Resolver> {
    Superblock::create(
        SuperblockLogConfig {
            path: path.to_path_buf(),
            rotate_threshold: 1_000_000,
            append_log_options: pi_append_log::AppendOptions { durable: true },
        },
        devices
            .iter()
            .map(|device| InitialDevice {
                label: DeviceLabel::new(format!("d{}", device.key.raw())).expect("label"),
                device: device.clone(),
            })
            .collect(),
        Arc::new(Resolver::default()),
        Arc::new(Codec),
        State(1),
    )
    .await
    .expect("create")
}
