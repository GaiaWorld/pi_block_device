use std::collections::BTreeMap;
use std::future::{Future, ready};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use pi_append_log::AppendOptions;
use pi_block_device::{
    BackendCapabilities, BlockDeviceBackend, BlockDeviceId, BlockDeviceState,
    BlockDeviceWriteBatch, BlockId, BlockWriteChunk, DeviceError, DeviceKey, DeviceOperation,
    InspectReport, ProtectedReadBuf, ReconcileError, ReconcileProgress, ReconcileReport,
    RetiredObject, RetirementReport,
};
use pi_superblock::{
    ApplicationApplyContext, ApplicationApplyError, ApplicationCodec, ApplicationCodecError,
    ApplicationCodecFailureKind, ApplicationCodecOperation, ApplicationEncoder, ApplicationState,
    DeviceLabel, DeviceRegistration, DeviceResolver, InitialDevice, ResolveError, Superblock,
    SuperblockLogConfig,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestState {
    pub value: u64,
    pub roots: usize,
    pub bindings: usize,
}

#[derive(Clone, Debug)]
pub struct TestDelta(pub u64);

impl ApplicationState for TestState {
    type Delta = TestDelta;

    fn apply(
        &self,
        delta: &Self::Delta,
        context: &ApplicationApplyContext<'_>,
    ) -> Result<Self, ApplicationApplyError> {
        Ok(Self {
            value: delta.0,
            roots: context.roots().count(),
            bindings: context.bindings().count(),
        })
    }
}

#[derive(Default)]
pub struct TestCodec {
    fail_delta: std::sync::atomic::AtomicBool,
}

impl TestCodec {
    pub fn set_fail_delta(&self, fail: bool) {
        self.fail_delta
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }
}

impl ApplicationCodec<TestState> for TestCodec {
    fn encode_state(
        &self,
        state: &TestState,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u64_le(state.value)?;
        output.put_u64_le(state.roots as u64)?;
        output.put_u64_le(state.bindings as u64)
    }

    fn decode_state(&self, bytes: Bytes) -> Result<TestState, ApplicationCodecError> {
        if bytes.len() != 24 {
            return Err(codec_error("invalid state length"));
        }
        Ok(TestState {
            value: u64::from_le_bytes(bytes[0..8].try_into().expect("value bytes")),
            roots: u64::from_le_bytes(bytes[8..16].try_into().expect("root bytes")) as usize,
            bindings: u64::from_le_bytes(bytes[16..24].try_into().expect("binding bytes")) as usize,
        })
    }

    fn encode_delta(
        &self,
        delta: &TestDelta,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        if self.fail_delta.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(codec_error("injected delta encode failure"));
        }
        output.put_u64_le(delta.0)
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<TestDelta, ApplicationCodecError> {
        if bytes.len() != 8 {
            return Err(codec_error("invalid delta length"));
        }
        Ok(TestDelta(u64::from_le_bytes(
            bytes[..8].try_into().expect("delta bytes"),
        )))
    }
}

fn codec_error(message: &'static str) -> ApplicationCodecError {
    ApplicationCodecError::from_message(
        ApplicationCodecOperation::EncodeDelta,
        ApplicationCodecFailureKind::Encode,
        message,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    None,
    Begin,
    Write,
    Prepare,
}

#[derive(Default, Debug)]
pub struct Calls {
    pub begins: usize,
    pub writes: usize,
    pub prepares: usize,
    pub drops: usize,
    pub events: Vec<(DeviceKey, &'static str)>,
}

pub struct TestBackend {
    id: BlockDeviceId,
    key: DeviceKey,
    failure: Arc<Mutex<Failure>>,
    pub calls: Arc<Mutex<Calls>>,
}

impl TestBackend {
    pub fn new(key: DeviceKey) -> Self {
        Self {
            id: BlockDeviceId::from_bytes([key.raw() as u8; 16]).expect("device id"),
            key,
            failure: Arc::new(Mutex::new(Failure::None)),
            calls: Arc::new(Mutex::new(Calls::default())),
        }
    }

    pub fn fail(&self, failure: Failure) {
        *self.failure.lock().expect("failure") = failure;
    }
}

pub struct TestBatch {
    key: DeviceKey,
    failure: Arc<Mutex<Failure>>,
    calls: Arc<Mutex<Calls>>,
}

fn device_error(operation: DeviceOperation, key: DeviceKey) -> DeviceError {
    DeviceError {
        operation,
        device_key: Some(key),
        source: Box::new(io::Error::other("injected device failure")),
    }
}

impl BlockDeviceWriteBatch for TestBatch {
    fn write(
        &mut self,
        _: BlockWriteChunk,
    ) -> impl Future<Output = Result<(), DeviceError>> + Send + '_ {
        self.calls.lock().expect("calls").writes += 1;
        let fail = *self.failure.lock().expect("failure") == Failure::Write;
        ready(if fail {
            Err(device_error(DeviceOperation::Write, self.key))
        } else {
            Ok(())
        })
    }

    fn prepare(self) -> impl Future<Output = Result<(), DeviceError>> + Send {
        let mut calls = self.calls.lock().expect("calls");
        calls.prepares += 1;
        calls.events.push((self.key, "prepare"));
        let fail = *self.failure.lock().expect("failure") == Failure::Prepare;
        ready(if fail {
            Err(device_error(DeviceOperation::Prepare, self.key))
        } else {
            Ok(())
        })
    }
}
impl Drop for TestBatch {
    fn drop(&mut self) {
        self.calls.lock().expect("calls").drops += 1;
    }
}

pub struct TestLease;
pub struct TestReadBuf(TestLease);
impl AsRef<[u8]> for TestReadBuf {
    fn as_ref(&self) -> &[u8] {
        &[]
    }
}
impl ProtectedReadBuf for TestReadBuf {
    type Lease = TestLease;
    fn lease(&self) -> &Self::Lease {
        &self.0
    }
}

impl BlockDeviceBackend for TestBackend {
    type SnapshotLease = TestLease;
    type ReadBuf = TestReadBuf;
    type WriteBatch = TestBatch;

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
    fn begin_write_batch(self: &Arc<Self>) -> Result<Self::WriteBatch, DeviceError> {
        self.calls.lock().expect("calls").begins += 1;
        if *self.failure.lock().expect("failure") == Failure::Begin {
            Err(device_error(DeviceOperation::BeginWriteBatch, self.key))
        } else {
            Ok(TestBatch {
                key: self.key,
                failure: Arc::clone(&self.failure),
                calls: Arc::clone(&self.calls),
            })
        }
    }
    fn acquire_snapshot_lease(
        self: &Arc<Self>,
        _: Arc<BlockDeviceState>,
    ) -> Result<Self::SnapshotLease, DeviceError> {
        Ok(TestLease)
    }
    fn read<'a>(
        self: &'a Arc<Self>,
        _: &'a Self::SnapshotLease,
        _: BlockId,
    ) -> impl Future<Output = Result<Self::ReadBuf, DeviceError>> + Send + 'a {
        ready(Ok(TestReadBuf(TestLease)))
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
    fn enqueue_retirements(self: &Arc<Self>, _: Vec<RetiredObject>) -> Result<(), DeviceError> {
        Ok(())
    }
    async fn reclaim_retirements(self: &Arc<Self>) -> Result<RetirementReport, DeviceError> {
        Ok(RetirementReport {
            attempted: 0,
            completed: 0,
            guarded: 0,
            retryable: 0,
        })
    }
}

#[derive(Default)]
pub struct TestResolver {
    devices: BTreeMap<DeviceKey, Arc<TestBackend>>,
}

impl TestResolver {
    pub fn new(devices: &[Arc<TestBackend>]) -> Self {
        Self {
            devices: devices
                .iter()
                .map(|device| (device.key, device.clone()))
                .collect(),
        }
    }
}

impl DeviceResolver<TestBackend> for TestResolver {
    fn resolve<'a>(
        &'a self,
        key: DeviceKey,
        _: &'a DeviceRegistration,
    ) -> impl Future<Output = Result<Arc<TestBackend>, ResolveError>> + Send + 'a {
        ready(self.devices.get(&key).cloned().ok_or_else(|| ResolveError {
            device_key: key,
            source: Box::new(io::Error::other("missing test device")),
        }))
    }
}

pub type TestSuperblock = Superblock<TestState, TestCodec, TestBackend, TestResolver>;

pub fn key(raw: u32) -> DeviceKey {
    DeviceKey::new(raw).expect("device key")
}

pub fn log_config(path: &Path) -> SuperblockLogConfig {
    log_config_with_threshold(path, 1024 * 1024)
}

pub fn log_config_with_threshold(path: &Path, rotate_threshold: u64) -> SuperblockLogConfig {
    SuperblockLogConfig {
        path: path.to_path_buf(),
        rotate_threshold,
        append_log_options: AppendOptions { durable: true },
    }
}

pub fn create(path: &Path, devices: &[Arc<TestBackend>], codec: Arc<TestCodec>) -> TestSuperblock {
    create_with_threshold(path, devices, codec, 1024 * 1024)
}

pub fn create_with_threshold(
    path: &Path,
    devices: &[Arc<TestBackend>],
    codec: Arc<TestCodec>,
    rotate_threshold: u64,
) -> TestSuperblock {
    let initial = devices
        .iter()
        .map(|device| InitialDevice {
            label: DeviceLabel::new(format!("device-{}", device.key.raw())).expect("label"),
            device: device.clone(),
        })
        .collect();
    futures_lite::future::block_on(TestSuperblock::create(
        log_config_with_threshold(path, rotate_threshold),
        initial,
        Arc::new(TestResolver::new(devices)),
        codec,
        TestState {
            value: 0,
            roots: 0,
            bindings: 0,
        },
    ))
    .expect("create superblock")
}

pub fn open(path: &Path, devices: &[Arc<TestBackend>], codec: Arc<TestCodec>) -> TestSuperblock {
    futures_lite::future::block_on(TestSuperblock::open(
        log_config(path),
        Arc::new(TestResolver::new(devices)),
        codec,
    ))
    .expect("open superblock")
}
