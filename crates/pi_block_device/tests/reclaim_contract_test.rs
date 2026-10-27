// @test @api: BlockDeviceBackend::reclaim_retirements
// @design-doc: design/features/{03-backend-session,04-reconcile-retire,07-snapshot-blockref}.feature
// Deterministic protocol mock only, not evidence for a production reclamation engine.
use std::future::{Future, ready};
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use pi_block_device::{
    BackendCapabilities, BlockDeviceBackend, BlockDeviceId, BlockDeviceState,
    BlockDeviceWriteBatch, BlockId, BlockWriteChunk, DeviceError, DeviceOperation, InspectReport,
    LargeBlockId, ProtectedReadBuf, ReconcileError, ReconcileProgress, ReconcileReport,
    RetiredObject, RetirementReport,
};

struct Lease(Arc<BlockDeviceState>);
struct ReadBuf {
    bytes: Bytes,
    lease: Lease,
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

struct Batch;
impl BlockDeviceWriteBatch for Batch {
    fn write(
        &mut self,
        _: BlockWriteChunk,
    ) -> impl Future<Output = Result<(), DeviceError>> + Send + '_ {
        ready(Ok(()))
    }
    fn prepare(self) -> impl Future<Output = Result<(), DeviceError>> + Send {
        ready(Ok(()))
    }
}

#[derive(Default)]
struct Model {
    queue: Vec<RetiredObject>,
    guards: Vec<std::sync::Weak<BlockDeviceState>>,
}
#[derive(Default)]
struct Backend(Mutex<Model>);
impl BlockDeviceBackend for Backend {
    type SnapshotLease = Lease;
    type ReadBuf = ReadBuf;
    type WriteBatch = Batch;
    fn device_id(&self) -> BlockDeviceId {
        BlockDeviceId::from_bytes([1; 16]).unwrap()
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
        state: Arc<BlockDeviceState>,
    ) -> Result<Lease, DeviceError> {
        self.0.lock().unwrap().guards.push(Arc::downgrade(&state));
        Ok(Lease(state))
    }
    fn read<'a>(
        self: &'a Arc<Self>,
        lease: &'a Lease,
        _: BlockId,
    ) -> impl Future<Output = Result<ReadBuf, DeviceError>> + Send + 'a {
        ready(Ok(ReadBuf {
            bytes: Bytes::from_static(b"data"),
            lease: Lease(Arc::clone(&lease.0)),
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
    fn enqueue_retirements(
        self: &Arc<Self>,
        objects: Vec<RetiredObject>,
    ) -> Result<(), DeviceError> {
        let mut model = self.0.lock().unwrap();
        for object in objects {
            if !model.queue.contains(&object) {
                model.queue.push(object);
            }
        }
        Ok(())
    }
    async fn reclaim_retirements(self: &Arc<Self>) -> Result<RetirementReport, DeviceError> {
        // All mock work completes in one poll; this lock serializes model selection and outcomes.
        let mut model = self.0.lock().unwrap();
        let selected = std::mem::take(&mut model.queue);
        let mut report = RetirementReport {
            attempted: 0,
            completed: 0,
            guarded: 0,
            retryable: 0,
        };
        for object in selected {
            report.attempted += 1;
            let guarded = model
                .guards
                .iter()
                .filter_map(std::sync::Weak::upgrade)
                .any(|state| match object {
                    RetiredObject::LargeBlock { id } => {
                        state.large_blocks.entries.contains_key(&id)
                    }
                    RetiredObject::Segment { level, version } => state.levels[usize::from(level)]
                        .versions
                        .get(
                            (version.raw() - state.levels[usize::from(level)].min_version.raw())
                                as usize,
                        )
                        .is_some_and(Option::is_some),
                });
            if guarded {
                report.guarded += 1;
                model.queue.push(object);
            } else if object == retired(2) {
                // An injected unresolved single-object outcome retains responsibility.
                report.retryable += 1;
                model.queue.push(object);
            } else {
                // Model evidence confirms absence, with no physical delete performed.
                report.completed += 1;
            }
        }
        Ok(report)
    }
}

fn retired(raw: u64) -> RetiredObject {
    RetiredObject::LargeBlock {
        id: LargeBlockId::new(raw).unwrap(),
    }
}
fn poll_ready<F: Future + Send>(future: F) -> F::Output {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("deterministic mock must complete without waiting"),
    }
}

#[test]
fn mock_empty_round_returns_all_zero_and_operation_name_is_stable() {
    let backend = Arc::new(Backend::default());
    let report = poll_ready(backend.reclaim_retirements()).unwrap();
    assert_eq!(
        report,
        RetirementReport {
            attempted: 0,
            completed: 0,
            guarded: 0,
            retryable: 0
        }
    );
    assert_eq!(
        DeviceOperation::ReclaimRetirements.to_string(),
        "reclaim_retirements"
    );
    backend.begin_write_batch().unwrap();
}

#[test]
fn mock_reports_guarded_retryable_and_confirmed_absence_without_dropping_responsibility() {
    let backend = Arc::new(Backend::default());
    let mut state = BlockDeviceState::initial().unwrap();
    state.large_blocks.next_id = pi_block_device::LargeBlockCursor::new(2).unwrap();
    state.large_blocks.entries.insert_mut(
        LargeBlockId::new(1).unwrap(),
        pi_block_device::LargeBlockData {
            allocated_at: pi_block_device::RecordSeq::new(1).unwrap(),
            released_at: pi_block_device::RecordSeq::NULL,
            logical_len: 4,
        },
    );
    let lease = backend.acquire_snapshot_lease(Arc::new(state)).unwrap();
    let read =
        poll_ready(backend.read(&lease, BlockId::large(LargeBlockId::new(1).unwrap()))).unwrap();
    backend
        .enqueue_retirements(vec![retired(1), retired(2), retired(3), retired(1)])
        .unwrap();
    assert_eq!(
        backend.0.lock().unwrap().queue,
        [retired(1), retired(2), retired(3)]
    );
    let report = poll_ready(backend.reclaim_retirements()).unwrap();
    assert_eq!(
        report,
        RetirementReport {
            attempted: 3,
            completed: 1,
            guarded: 1,
            retryable: 1
        }
    );
    assert_eq!(
        report.attempted,
        report.completed + report.guarded + report.retryable
    );
    assert_eq!(backend.0.lock().unwrap().queue, [retired(1), retired(2)]);
    drop(lease);
    assert_eq!(
        poll_ready(backend.reclaim_retirements()).unwrap().guarded,
        1
    );
    assert_eq!(read.as_ref(), b"data");
    drop(read);
    assert_eq!(
        poll_ready(backend.reclaim_retirements()).unwrap(),
        RetirementReport {
            attempted: 2,
            completed: 1,
            guarded: 0,
            retryable: 1
        }
    );
    assert_eq!(backend.0.lock().unwrap().queue, [retired(2)]);
}
