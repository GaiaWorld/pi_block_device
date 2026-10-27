//! @test @api: public batch cancellation and serialization with real Local I/O
//! @impl: src/write_impl.rs
//! @design-doc: design/features/03-backend-session.feature
use super::*;
use futures_lite::future::block_on;
use pi_block_device::{BlockWritePosition, SegmentVersion};
use std::sync::atomic::Ordering;
use std::task::Poll;

struct Pause {
    root: PathBuf,
    prepare: bool,
    reached: Arc<AtomicBool>,
}
static PAUSES: Mutex<Vec<Pause>> = Mutex::new(Vec::new());

pub(super) async fn pause(device: &FileBlockDevice, prepare: bool) {
    let reached = PAUSES
        .lock()
        .expect("hooks")
        .iter()
        .find(|hook| hook.root == device.root && hook.prepare == prepare)
        .map(|hook| Arc::clone(&hook.reached));
    if let Some(reached) = reached {
        reached.store(true, Ordering::Release);
        std::future::pending::<()>().await;
    }
}

fn device(root: PathBuf) -> Arc<FileBlockDevice> {
    // SAFETY: isolated root; only coordinated backend I/O.
    block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root }) }).expect("create")
}
fn request(index: u32) -> BlockWriteChunk {
    BlockWriteChunk::new(
        BlockId::ordinary(0, SegmentVersion::new(1).expect("version"), index).expect("slot"),
        Bytes::from(vec![5; 4096]),
        BlockWritePosition::new(0, 4096).expect("position"),
    )
    .expect("chunk")
}

#[test]
fn cancelled_write_or_prepare_keeps_real_bytes_and_fences_next_batch() {
    for prepare in [false, true] {
        let temp = tempfile::tempdir().expect("root");
        let device = device(temp.path().join("device"));
        let reached = Arc::new(AtomicBool::new(false));
        PAUSES.lock().expect("hooks").push(Pause {
            root: device.root.clone(),
            prepare,
            reached: Arc::clone(&reached),
        });
        let mut batch = device.begin_write_batch().expect("begin");
        if prepare {
            block_on(batch.write(request(0))).expect("real append");
            let mut future = Box::pin(batch.prepare());
            block_on(std::future::poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
                if reached.load(Ordering::Acquire) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }));
            drop(future);
        } else {
            let mut future = Box::pin(batch.write(request(0)));
            block_on(std::future::poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
                if reached.load(Ordering::Acquire) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }));
            drop(future);
            assert!(block_on(batch.write(request(0))).is_err());
            drop(batch);
        }
        PAUSES
            .lock()
            .expect("hooks")
            .retain(|hook| hook.root != device.root);
        assert!(device.begin_write_batch().is_err());
        assert_eq!(
            std::fs::read(device.root.join("segment-00-00000001.bin")).expect("preserved"),
            vec![5; 4096]
        );
    }
}

#[test]
fn concurrent_write_waits_and_cancelled_waiter_leaves_no_reservation() {
    let temp = tempfile::tempdir().expect("root");
    let device = device(temp.path().join("device"));
    let mut first = device.begin_write_batch().expect("first");
    block_on(first.write(request(0))).expect("slot zero");
    let mut second = device.begin_write_batch().expect("begin does not wait");
    let mut waiting = Box::pin(second.write(request(1)));
    assert!(block_on(futures_lite::future::poll_once(waiting.as_mut())).is_none());
    drop(waiting);
    drop(first);
    // Waiting cancellation neither fences the device nor reserves the gate.
    drop(device.begin_write_batch().expect("not fenced"));
    block_on(second.write(request(1))).expect("slot one");
    drop(second);
    assert_eq!(
        std::fs::metadata(device.root.join("segment-00-00000001.bin"))
            .expect("len")
            .len(),
        8192
    );
}

#[test]
fn batch_created_before_partial_writer_drop_rechecks_fence_on_first_write() {
    let temp = tempfile::tempdir().expect("root");
    let device = device(temp.path().join("device"));
    let mut first = device.begin_write_batch().expect("first");
    let mut waiting = device.begin_write_batch().expect("before fence");
    let partial = BlockWriteChunk::new(
        request(0).block(),
        Bytes::from_static(b"prefix"),
        BlockWritePosition::new(0, 4096).unwrap(),
    )
    .unwrap();
    block_on(first.write(partial)).expect("partial I/O");
    drop(first);
    let error = block_on(waiting.write(request(1))).expect_err("lock-time fence");
    assert_eq!(error.operation, DeviceOperation::Write);
    assert_eq!(
        std::fs::read(device.root.join("segment-00-00000001.bin")).unwrap(),
        b"prefix"
    );
}

#[test]
fn cancelled_recovery_after_real_truncate_fences_and_reopen_resyncs() {
    let temp = tempfile::tempdir().expect("root");
    let device = device(temp.path().join("device"));
    let config = FileBlockDeviceConfig {
        root: device.root.clone(),
    };
    let id = device.device_id();
    let mut state = BlockDeviceState::initial().unwrap();
    let mut data = state.levels[0].versions.get(0).unwrap().clone().unwrap();
    data.allocation_runs
        .push_back_mut(pi_block_device::AllocationRun {
            allocated_at: pi_block_device::RecordSeq::new(1).unwrap(),
            len: 1,
        });
    data.released_at
        .push_back_mut(pi_block_device::RecordSeq::NULL);
    state.levels[0].versions.set_mut(0, Some(data));
    assert!(state.validate());
    let state = Arc::new(state);
    let path = device.root.join("segment-00-00000001.bin");
    std::fs::write(&path, [vec![4; 4096], vec![7; 2]].concat()).unwrap();
    let reached = Arc::new(AtomicBool::new(false));
    PAUSES.lock().unwrap().push(Pause {
        root: device.root.clone(),
        prepare: true,
        reached: Arc::clone(&reached),
    });
    let mut future = Box::pin(device.reconcile(Arc::clone(&state)));
    block_on(std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        if reached.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }));
    // Actual truncate completed, but sync has not started and no report exists.
    assert!(device.acquire_snapshot_lease(Arc::clone(&state)).is_err());
    drop(future);
    PAUSES
        .lock()
        .unwrap()
        .retain(|hook| hook.root != device.root);
    assert!(device.begin_write_batch().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), vec![4; 4096]);
    drop(device);
    // SAFETY: all earlier operations released; isolated root.
    let device = block_on(unsafe { FileBlockDevice::open(config, Some(id)) }).unwrap();
    let report =
        block_on(device.reconcile(Arc::clone(&state))).expect("fresh sync after cancellation");
    assert_eq!(report.progress.planned_actions, 0);
    assert!(report.progress.media_metadata_synced);
    let lease = device.acquire_snapshot_lease(state).unwrap();
    let prefix = block_on(device.read(&lease, request(0).block())).unwrap();
    assert_eq!(prefix.as_ref(), vec![4; 4096]);
    drop(device.begin_write_batch().unwrap());
}

#[test]
fn cancelled_virtual_active_cleanup_never_releases_admission_or_write_fence() {
    for after_delete in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let device = device(temp.path().join("device"));
        let path = device.root.join("segment-00-00000001.bin");
        std::fs::write(&path, [9, 9]).unwrap();
        let reached = Arc::new(AtomicBool::new(false));
        PAUSES.lock().unwrap().push(Pause {
            root: device.root.clone(),
            prepare: after_delete,
            reached: Arc::clone(&reached),
        });
        let expected = Arc::new(BlockDeviceState::initial().unwrap());
        let mut future = Box::pin(device.reconcile(Arc::clone(&expected)));
        block_on(std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            if reached.load(Ordering::Acquire) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }));
        // Before submission really retains bytes; after awaiting remove really has no file.
        // Neither cancelling point is evidence that a native pending remove did not commit.
        assert_eq!(path.exists(), !after_delete);
        drop(future);
        PAUSES
            .lock()
            .unwrap()
            .retain(|hook| hook.root != device.root);
        assert!(device.begin_write_batch().is_err());
        let object = RetiredObject::Segment {
            level: 0,
            version: SegmentVersion::new(1).unwrap(),
        };
        assert!(device.retirement_gate.lock().unwrap().contains(&object));
        let mut live = BlockDeviceState::initial().unwrap();
        let mut data = live.levels[0].versions.get(0).unwrap().clone().unwrap();
        data.allocation_runs
            .push_back_mut(pi_block_device::AllocationRun {
                allocated_at: pi_block_device::RecordSeq::new(1).unwrap(),
                len: 1,
            });
        data.released_at
            .push_back_mut(pi_block_device::RecordSeq::NULL);
        live.levels[0].versions.set_mut(0, Some(data));
        assert!(device.acquire_snapshot_lease(Arc::new(live)).is_err());
        // Explicit retry may finish cleanup, but cannot clear prior unsafe admission/fence.
        block_on(device.reconcile(expected)).unwrap();
        assert!(device.retirement_gate.lock().unwrap().contains(&object));
        assert!(device.begin_write_batch().is_err());
    }
}

#[test]
fn unresolved_virtual_active_removal_is_not_retried_or_unfenced() {
    let temp = tempfile::tempdir().unwrap();
    let device = device(temp.path().join("device"));
    let path = device.root.join("segment-00-00000001.bin");
    std::fs::write(&path, [9, 9]).unwrap();
    let object = RetiredObject::Segment {
        level: 0,
        version: SegmentVersion::new(1).unwrap(),
    };
    // Internal unresolved-evidence fixture, not a claimed native Local Unknown outcome.
    device.retirement_gate.lock().unwrap().push(object);
    device.unresolved_removals.lock().unwrap().push(object);
    let error = block_on(device.reconcile(Arc::new(BlockDeviceState::initial().unwrap())))
        .expect_err("unresolved remove cannot authorize unlink");
    assert_eq!(
        error.kind,
        pi_block_device::ReconcileFailureKind::ConcurrentMutation
    );
    assert!(!error.progress.media_metadata_synced);
    assert_eq!(std::fs::read(path).unwrap(), [9, 9]);
    assert!(device.retirement_gate.lock().unwrap().contains(&object));
    assert!(device.unresolved_removals.lock().unwrap().contains(&object));
    assert!(device.begin_write_batch().is_err());
}
