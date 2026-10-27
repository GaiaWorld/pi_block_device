use bytes::Bytes;
use pi_block_device::*;
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig};
use pi_superblock::*;
use std::future::{Future, poll_fn};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Poll, Waker};

#[derive(Clone)]
pub struct App;
impl ApplicationState for App {
    type Delta = ();
}
pub struct Codec;
impl ApplicationCodec<App> for Codec {
    fn encode_state(
        &self,
        _: &App,
        out: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        out.put_slice(b"APP")
    }
    fn decode_state(&self, bytes: Bytes) -> Result<App, ApplicationCodecError> {
        if bytes.as_ref() != b"APP" {
            return Err(ApplicationCodecError::new("invalid APP"));
        }
        Ok(App)
    }
    fn encode_delta(
        &self,
        _: &(),
        _: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
    }
    fn decode_delta(&self, bytes: Bytes) -> Result<(), ApplicationCodecError> {
        if !bytes.is_empty() {
            return Err(ApplicationCodecError::new("invalid delta"));
        }
        Ok(())
    }
}

pub struct Gate {
    sender: mpsc::Sender<bool>,
    receiver: Mutex<mpsc::Receiver<bool>>,
    waker: Mutex<Option<Waker>>,
}
impl Gate {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver: Mutex::new(receiver),
            waker: Mutex::new(None),
        }
    }
    pub fn release(&self, success: bool) {
        self.sender.send(success).unwrap();
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
    async fn wait(&self) -> bool {
        poll_fn(|cx| {
            let mut waker = self.waker.lock().unwrap();
            *waker = Some(cx.waker().clone());
            match self.receiver.lock().unwrap().try_recv() {
                Ok(success) => Poll::Ready(success),
                Err(mpsc::TryRecvError::Empty) => Poll::Pending,
                Err(mpsc::TryRecvError::Disconnected) => panic!("gate disconnected"),
            }
        })
        .await
    }
}

pub type Events = Arc<Mutex<Vec<(u32, &'static str)>>>;
type Observer = Box<dyn Fn(&[RetiredObject]) + Send + Sync>;
pub struct Backend {
    pub file: Arc<FileBlockDevice>,
    pub key: DeviceKey,
    pub gate: Arc<Gate>,
    pub events: Events,
    pub observer: Mutex<Option<Observer>>,
}
pub struct Batch {
    inner: <FileBlockDevice as BlockDeviceBackend>::WriteBatch,
    owner: Arc<Backend>,
}
impl Backend {
    pub fn wrap(file: Arc<FileBlockDevice>, key: DeviceKey, events: Events) -> Arc<Self> {
        Arc::new(Self {
            file,
            key,
            gate: Arc::new(Gate::new()),
            events,
            observer: Mutex::new(None),
        })
    }
}
impl BlockDeviceWriteBatch for Batch {
    async fn write(&mut self, chunk: BlockWriteChunk) -> Result<(), DeviceError> {
        self.inner.write(chunk).await
    }
    async fn prepare(self) -> Result<(), DeviceError> {
        self.owner
            .events
            .lock()
            .unwrap()
            .push((self.owner.key.raw(), "prepare"));
        if !self.owner.gate.wait().await {
            return Err(DeviceError {
                operation: DeviceOperation::Prepare,
                device_key: Some(self.owner.key),
                source: Box::new(std::io::Error::other("controlled prepare failure")),
            });
        }
        self.inner.prepare().await?;
        self.owner
            .events
            .lock()
            .unwrap()
            .push((self.owner.key.raw(), "prepared"));
        Ok(())
    }
}
impl BlockDeviceBackend for Backend {
    type SnapshotLease = <FileBlockDevice as BlockDeviceBackend>::SnapshotLease;
    type ReadBuf = <FileBlockDevice as BlockDeviceBackend>::ReadBuf;
    type WriteBatch = Batch;
    fn device_id(&self) -> BlockDeviceId {
        self.file.device_id()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.file.capabilities()
    }
    async fn inspect(&self) -> Result<InspectReport, DeviceError> {
        self.file.inspect().await
    }
    fn begin_write_batch(self: &Arc<Self>) -> Result<Batch, DeviceError> {
        let inner = self.file.begin_write_batch()?;
        self.events.lock().unwrap().push((self.key.raw(), "begin"));
        Ok(Batch {
            inner,
            owner: Arc::clone(self),
        })
    }
    fn acquire_snapshot_lease(
        self: &Arc<Self>,
        state: Arc<BlockDeviceState>,
    ) -> Result<Self::SnapshotLease, DeviceError> {
        self.file.acquire_snapshot_lease(state)
    }
    async fn read<'a>(
        self: &'a Arc<Self>,
        lease: &'a Self::SnapshotLease,
        block: BlockId,
    ) -> Result<Self::ReadBuf, DeviceError> {
        self.file.read(lease, block).await
    }
    async fn reconcile(
        self: &Arc<Self>,
        state: Arc<BlockDeviceState>,
    ) -> Result<ReconcileReport, ReconcileError> {
        self.file.reconcile(state).await
    }
    fn enqueue_retirements(
        self: &Arc<Self>,
        retired: Vec<RetiredObject>,
    ) -> Result<(), DeviceError> {
        if let Some(observer) = self.observer.lock().unwrap().as_ref() {
            observer(&retired);
        }
        self.events.lock().unwrap().push((self.key.raw(), "retire"));
        self.file.enqueue_retirements(retired)
    }
    async fn reclaim_retirements(self: &Arc<Self>) -> Result<RetirementReport, DeviceError> {
        self.file.reclaim_retirements().await
    }
}
pub struct Resolver(pub Vec<Arc<Backend>>);
impl DeviceResolver<Backend> for Resolver {
    async fn resolve(
        &self,
        key: DeviceKey,
        registration: &DeviceRegistration,
    ) -> Result<Arc<Backend>, ResolveError> {
        let device = Arc::clone(&self.0[key.raw() as usize - 1]);
        assert_eq!(device.device_id(), registration.id);
        Ok(device)
    }
}
pub type Owner = Superblock<App, Codec, Backend, Resolver>;
pub fn key(raw: u32) -> DeviceKey {
    DeviceKey::new(raw).unwrap()
}
pub fn config(path: &Path) -> SuperblockLogConfig {
    SuperblockLogConfig {
        path: path.to_owned(),
        rotate_threshold: 1_000_000,
        append_log_options: pi_append_log::AppendOptions { durable: true },
    }
}
pub async fn fixture() -> (tempfile::TempDir, Arc<Owner>, Vec<Arc<Backend>>) {
    let temp = tempfile::tempdir().unwrap();
    assert!(temp.path().is_absolute());
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut devices = Vec::new();
    for raw in 1..=3 {
        let root = temp.path().join(format!("device-{raw}"));
        assert!(!root.exists());
        // SAFETY: each absent root is inside this exclusively owned TempDir; no aliases or external mutations.
        let file = unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root }) }
            .await
            .unwrap();
        devices.push(Backend::wrap(file, key(raw), Arc::clone(&events)));
    }
    let sb = Owner::create(
        config(&temp.path().join("log")),
        devices
            .iter()
            .map(|device| InitialDevice {
                label: DeviceLabel::new(format!("device-{}", device.key.raw())).unwrap(),
                device: Arc::clone(device),
            })
            .collect(),
        Arc::new(Resolver(devices.clone())),
        Arc::new(Codec),
        App,
    )
    .await
    .unwrap();
    (temp, Arc::new(sb), devices)
}
pub async fn records(path: &Path) -> Vec<(u32, RecordKind)> {
    let mut records = Vec::new();
    drop(
        SuperblockAppendLog::open_replaying(path.to_owned(), u32::MAX as usize, |record| {
            records.push((record.record_seq.raw(), record.header.record_kind));
            false
        })
        .await
        .unwrap(),
    );
    records
}
pub async fn reach_gate<F: Future>(mut commit: Pin<&mut F>, events: &Events, raw: u32) {
    poll_fn(|cx| {
        assert!(
            commit.as_mut().poll(cx).is_pending(),
            "commit completed before controlled gate"
        );
        if events.lock().unwrap().contains(&(raw, "prepare")) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
}
pub fn image(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            assert!(path.is_file());
            (
                path.file_name().unwrap().into(),
                std::fs::read(path).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}
