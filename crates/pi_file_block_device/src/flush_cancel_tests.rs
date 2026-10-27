//! @test @api: public begin/write/prepare cancellation with real flush ownership
//! @impl: src/write_impl.rs
//! @design-doc: design/features/03-backend-session.feature
use super::*;
use futures_lite::future::{block_on, poll_once};
use pi_async_fs::{OverwriteTargetEvidence, TransferProgress};
use pi_block_device::{AllocationRun, BlockWritePosition, RecordSeq, SegmentVersion};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

type Files = std::collections::BTreeMap<PathBuf, LocalFile>;
static RETAIN: Mutex<Option<(PathBuf, mpsc::Sender<Files>)>> = Mutex::new(None);
const CHILD: &str = "PI_FILE_FLUSH_CANCEL_CHILD";
const EXACT: &str =
    "flush_cancel_tests::cancelled_prepare_retains_real_flush_lease_until_completion";
const LIMIT: Duration = Duration::from_secs(60);

pub(super) fn retain_cancelled_files(batch: &mut FileWriteBatch) {
    let mut hook = RETAIN.lock().unwrap();
    if hook
        .as_ref()
        .is_some_and(|(root, _)| *root == batch.device.root)
    {
        let (_, sender) = hook.take().unwrap();
        // Test-only ownership handoff, after the real Drop fence. The same LocalFile
        // lets metadata await the actor's file-slot lock, not an unrelated handle.
        sender.send(std::mem::take(&mut batch.files)).unwrap();
    }
}

fn slot() -> BlockId {
    BlockId::ordinary(0, SegmentVersion::new(1).unwrap(), 0).unwrap()
}

fn oracle() -> Arc<BlockDeviceState> {
    let mut state = BlockDeviceState::initial().unwrap();
    let mut data = state.levels[0].versions.get(0).unwrap().clone().unwrap();
    data.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: RecordSeq::new(1).unwrap(),
        len: 1,
    });
    data.released_at.push_back_mut(RecordSeq::NULL);
    state.levels[0].versions.set_mut(0, Some(data));
    assert!(state.validate());
    Arc::new(state)
}

fn conflicts(truncate: &mut LocalFile, overwrite: &mut LocalFile) {
    // A Pending probe is a failure, never awaited while the only worker is held.
    let error = block_on(poll_once(truncate.truncate(4096)))
        .expect("truncate must reject synchronously, not queue I/O")
        .expect_err("flush lease must exclude truncate");
    // pi_result is transitive only; inspect its typed current context's stable
    // public Display, not the report chain or native OS error text.
    assert_eq!(error.current_context().to_string(), "conflict");
    let failure = block_on(poll_once(overwrite.overwrite_all(vec![0xEE; 4096])))
        .expect("overwrite must reject synchronously, not queue I/O")
        .expect_err("flush lease must exclude overwrite");
    let (error, bytes, progress, evidence) = failure.into_parts();
    assert_eq!(error.current_context().to_string(), "conflict");
    assert_eq!(bytes, vec![0xEE; 4096]);
    assert_eq!(progress, TransferProgress::Exact { bytes: 0 });
    assert_eq!(evidence, OverwriteTargetEvidence::Unchanged);
}

#[test]
fn cancelled_prepare_retains_real_flush_lease_until_completion() {
    if std::env::var(CHILD).as_deref() != Ok("active") {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", EXACT, "--nocapture"])
            .env(CHILD, "active")
            .env("BLOCKING_MAX_THREADS", "1")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (sender, receiver) = mpsc::channel();
        let mut stdout = child.stdout.take().unwrap();
        let waiter = std::thread::spawn(move || {
            let mut output = String::new();
            let result = stdout.read_to_string(&mut output);
            let _ = sender.send((result, output));
        });
        // No sleep/poll loop and no time-based I/O stage inference.
        let result = receiver.recv_timeout(LIMIT);
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            waiter.join().unwrap();
            panic!("child timed out; no I/O completion inferred");
        }
        let (read, output) = result.unwrap();
        read.unwrap();
        let status = child.wait().unwrap();
        waiter.join().unwrap();
        print!("{output}");
        assert!(status.success(), "isolated cancellation acceptance failed");
        assert!(output.contains("EVIDENCE writes_complete"));
        return;
    }
    assert_eq!(std::env::var("BLOCKING_MAX_THREADS").unwrap(), "1");
    let temporary = tempfile::tempdir().unwrap();
    let config = FileBlockDeviceConfig {
        root: temporary.path().join("device"),
    };
    // SAFETY: isolated root; all mutations use the shared dependency registry.
    let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
    let id = device.device_id();
    let payload = vec![0x53; 6];
    let mut expected_bytes = payload.clone();
    expected_bytes.resize(4096, 0);
    let expected = oracle(); // Independent fixture intent, not media-derived State.
    let mut batch = device.begin_write_batch().unwrap();
    block_on(
        batch.write(
            BlockWriteChunk::new(
                slot(),
                Bytes::from(payload),
                BlockWritePosition::new(0, 6).unwrap(),
            )
            .unwrap(),
        ),
    )
    .expect("actual payload and padding append completed");
    let path = config.root.join("segment-00-00000001.bin");
    assert_eq!(std::fs::read(&path).unwrap(), expected_bytes);
    // Preopen everything: open itself needs the blocking pool. The write API
    // already awaited both append actors and byte_len on the original handle.
    let mut truncate = block_on(device.namespace.open(&path, FileAccessMode::Truncate)).unwrap();
    let mut overwrite = block_on(device.namespace.open(&path, FileAccessMode::Overwrite)).unwrap();
    let (retained_sender, retained_receiver) = mpsc::channel();
    *RETAIN.lock().unwrap() = Some((config.root.clone(), retained_sender));
    let (started_sender, started_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let occupied = blocking::unblock(move || {
        started_sender.send(()).unwrap();
        release_receiver
            .recv_timeout(LIMIT)
            .expect("release worker");
    });
    started_receiver
        .recv_timeout(LIMIT)
        .expect("sole worker really started");
    let mut prepare = Box::pin(batch.prepare());
    assert!(block_on(poll_once(prepare.as_mut())).is_none());
    conflicts(&mut truncate, &mut overwrite); // Admission evidence, not Pending.
    drop(prepare); // Consuming future owns batch; actual Drop sets instance fence.
    let mut files = retained_receiver.recv_timeout(LIMIT).unwrap();
    let original = files.remove(&path).expect("original append resource");
    assert!(files.is_empty());
    assert!(device.begin_write_batch().is_err());
    conflicts(&mut truncate, &mut overwrite); // Worker still held after cancellation.
    assert_eq!(std::fs::read(&path).unwrap(), expected_bytes);
    release_sender.send(()).unwrap();
    block_on(occupied); // Only occupancy completion, NOT a FIFO flush barrier.
    // The original LocalFile slot is held by the detached flush actor until
    // sync_all finishes and its lease is dropped. byte_len locks that same slot.
    assert_eq!(block_on(original.byte_len()).unwrap(), 4096);
    block_on(truncate.truncate(4096)).expect("real exclusive admission restored, no byte change");
    block_on(original.byte_len()).expect("length remains readable after no-op truncate");
    assert_eq!(std::fs::read(&path).unwrap(), expected_bytes);
    block_on(truncate.flush(FileFlushMode::DataAndMetadata)).unwrap();
    assert!(device.begin_write_batch().is_err());
    drop((original, files, truncate, overwrite, device));
    // SAFETY: original flush and truncate completed; all earlier handles released.
    let reopened = block_on(unsafe { FileBlockDevice::open(config, Some(id)) }).unwrap();
    assert_eq!(reopened.device_id(), id);
    let report = block_on(reopened.reconcile(Arc::clone(&expected))).unwrap();
    assert_eq!(report.progress.planned_actions, 0);
    assert_eq!(report.progress.completed_actions, 0);
    assert_eq!(report.progress.remaining_actions, 0);
    assert!(!report.progress.partial_progress);
    assert!(report.progress.media_metadata_synced);
    assert_eq!(std::fs::read(&path).unwrap(), expected_bytes);
    let lease = reopened.acquire_snapshot_lease(expected).unwrap();
    let mapped = block_on(reopened.read(&lease, slot())).unwrap();
    assert_eq!(mapped.as_ref(), expected_bytes);
    drop(reopened.begin_write_batch().unwrap());
    println!(
        "EVIDENCE writes_complete worker_started prepare_flush_conflict cancel_drop_fence conflict_still_held original_handle_returned noop_truncate_completed reopen_synced exact_mmap"
    );
}
