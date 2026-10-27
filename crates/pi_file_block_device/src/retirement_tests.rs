//! @test: public finite reclaim with real dependency I/O; no workers or sleeps.
use super::*;
use futures_lite::future::block_on;
use pi_block_device::{LargeBlockCursor, LargeBlockData, LargeBlockId, RecordSeq};
use std::sync::atomic::Ordering;
use std::task::Poll;

struct Pause {
    root: PathBuf,
    admitted: bool,
    reached: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}
static PAUSES: Mutex<Vec<Pause>> = Mutex::new(Vec::new());

pub(super) async fn pause(device: &FileBlockDevice, admitted: bool) {
    let hook = PAUSES
        .lock()
        .expect("hooks")
        .iter()
        .find(|hook| hook.root == device.root && hook.admitted == admitted)
        .map(|hook| (Arc::clone(&hook.reached), Arc::clone(&hook.release)));
    if let Some((reached, release)) = hook {
        reached.store(true, Ordering::Release);
        std::future::poll_fn(|cx| {
            if release.load(Ordering::Acquire) {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }
}

fn install(device: &FileBlockDevice, admitted: bool) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
    let reached = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    PAUSES.lock().expect("hooks").push(Pause {
        root: device.root.clone(),
        admitted,
        reached: Arc::clone(&reached),
        release: Arc::clone(&release),
    });
    (reached, release)
}

fn clear(device: &FileBlockDevice) {
    PAUSES
        .lock()
        .expect("hooks")
        .retain(|hook| hook.root != device.root);
}

fn device(root: PathBuf) -> Arc<FileBlockDevice> {
    // SAFETY: isolated temporary roots; fixtures precede mapping. The invalid
    // parent test mutates only when no mapping or operation exists.
    block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root }) }).expect("create")
}
fn large(raw: u64) -> RetiredObject {
    RetiredObject::LargeBlock {
        id: LargeBlockId::new(raw).expect("id"),
    }
}
fn path(device: &FileBlockDevice, raw: u64) -> PathBuf {
    device.block_path(BlockId::large(LargeBlockId::new(raw).expect("id")))
}
fn state(ids: &[u64]) -> Arc<BlockDeviceState> {
    let mut state = BlockDeviceState::initial().expect("state");
    state.large_blocks.next_id = LargeBlockCursor::new(100).expect("cursor");
    for raw in ids {
        state.large_blocks.entries.insert_mut(
            LargeBlockId::new(*raw).expect("id"),
            LargeBlockData {
                allocated_at: RecordSeq::new(1).expect("seq"),
                released_at: RecordSeq::NULL,
                logical_len: 4,
            },
        );
    }
    Arc::new(state)
}
#[test]
fn missing_large_retirement_completes_and_deletes_following_object() {
    let parent = tempfile::tempdir().expect("parent");
    let device = device(parent.path().join("device"));
    std::fs::write(path(&device, 2), b"free").expect("fixture");
    device
        .enqueue_retirements(vec![large(1), large(2)])
        .expect("enqueue");
    let report = block_on(device.reclaim_retirements()).expect("round");
    assert_eq!((report.attempted, report.completed), (2, 2));
    assert!(!path(&device, 2).exists());
}
#[test]
fn deleted_large_reenqueue_completes_before_following_delete() {
    let parent = tempfile::tempdir().expect("parent");
    let device = device(parent.path().join("device"));
    std::fs::write(path(&device, 1), b"free").expect("fixture");
    device.enqueue_retirements(vec![large(1)]).expect("enqueue");
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("round")
            .completed,
        1
    );
    device
        .enqueue_retirements(vec![large(1), large(2)])
        .expect("reenqueue");
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("round")
            .completed,
        2
    );
}
#[test]
fn active_file_conflict_retains_responsibility_without_blocking_following_delete() {
    let parent = tempfile::tempdir().expect("parent");
    let device = device(parent.path().join("device"));
    std::fs::write(path(&device, 1), b"held").expect("fixture");
    std::fs::write(path(&device, 2), b"free").expect("fixture");
    let held = block_on(
        device
            .namespace
            .open(&path(&device, 1), FileAccessMode::Read),
    )
    .expect("held file");
    device
        .enqueue_retirements(vec![large(1), large(2)])
        .expect("enqueue");
    let report = block_on(device.reclaim_retirements()).expect("round");
    assert_eq!(
        (report.attempted, report.completed, report.retryable),
        (2, 1, 1)
    );
    assert!(path(&device, 1).exists());
    assert!(!path(&device, 2).exists());
    assert!(
        device.acquire_snapshot_lease(state(&[1])).is_err(),
        "admitted deletion rejects later lease"
    );
    drop(held);
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("retry")
            .completed,
        1
    );
}
#[test]
fn non_directory_parent_is_not_authoritative_absence() {
    let parent = tempfile::tempdir().expect("parent");
    let device = device(parent.path().join("device"));
    std::fs::remove_file(manifest_path(&device.root)).expect("fixture");
    std::fs::remove_dir(&device.root).expect("fixture");
    std::fs::write(&device.root, b"not a directory").expect("fixture");
    assert!(block_on(device.namespace.try_exists(&path(&device, 1))).is_err());
    device.enqueue_retirements(vec![large(1)]).expect("enqueue");
    let report = block_on(device.reclaim_retirements()).expect("round");
    assert_eq!((report.completed, report.retryable), (0, 1));
    assert_eq!(device.retirements.lock().expect("queue").len(), 1);
}
#[test]
fn guarded_head_does_not_block_unprotected_following_object() {
    let parent = tempfile::tempdir().expect("parent");
    let device = device(parent.path().join("device"));
    std::fs::write(path(&device, 1), b"held").expect("fixture");
    std::fs::write(path(&device, 2), b"free").expect("fixture");
    let lease = device.acquire_snapshot_lease(state(&[1])).expect("lease");
    let buffer = block_on(device.read(&lease, BlockId::large(LargeBlockId::new(1).expect("id"))))
        .expect("mmap");
    drop(lease);
    device
        .enqueue_retirements(vec![large(1), large(2)])
        .expect("enqueue");
    let report = block_on(device.reclaim_retirements()).expect("round");
    assert_eq!((report.guarded, report.completed), (1, 1));
    assert_eq!(buffer.as_ref(), b"held");
    drop(buffer);
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("released")
            .completed,
        1
    );
}
#[test]
fn fully_guarded_round_returns_and_concurrent_duplicate_enqueue_preserves_each_object() {
    let parent = tempfile::tempdir().expect("parent");
    let device = device(parent.path().join("device"));
    let lease = device
        .acquire_snapshot_lease(state(&[1, 2]))
        .expect("lease");
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let device = Arc::clone(&device);
            scope.spawn(move || {
                device
                    .enqueue_retirements(vec![large(1), large(2), large(1)])
                    .expect("enqueue")
            });
        }
    });
    let report = block_on(device.reclaim_retirements()).expect("finite guarded round");
    assert_eq!((report.attempted, report.guarded), (2, 2));
    assert_eq!(device.retirements.lock().expect("queue").len(), 2);
    drop(lease);
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("release")
            .completed,
        2
    );
}

#[test]
fn fixed_round_excludes_later_enqueue_and_deduplicates_inflight_object() {
    use std::future::Future;
    let parent = tempfile::tempdir().expect("parent");
    let device = device(parent.path().join("device"));
    std::fs::write(path(&device, 1), b"one").expect("fixture");
    std::fs::write(path(&device, 2), b"two").expect("fixture");
    device.enqueue_retirements(vec![large(1)]).expect("enqueue");
    let (reached, release) = install(&device, false);
    let mut round = Box::pin(device.reclaim_retirements());
    block_on(std::future::poll_fn(|cx| {
        assert!(round.as_mut().poll(cx).is_pending());
        if reached.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }));
    device
        .enqueue_retirements(vec![large(1), large(2), large(2)])
        .expect("later enqueue");
    release.store(true, Ordering::Release);
    let report = block_on(round).expect("round");
    clear(&device);
    assert_eq!((report.attempted, report.completed), (1, 1));
    assert!(path(&device, 2).exists());
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("next round")
            .completed,
        1
    );
}

#[test]
fn cancelled_admitted_delete_keeps_responsibility_and_rejects_new_lease() {
    use std::future::Future;
    let parent = tempfile::tempdir().expect("parent");
    let device = device(parent.path().join("device"));
    std::fs::write(path(&device, 1), b"held").expect("fixture");
    device.enqueue_retirements(vec![large(1)]).expect("enqueue");
    let (reached, _) = install(&device, true);
    let mut round = Box::pin(device.reclaim_retirements());
    block_on(std::future::poll_fn(|cx| {
        assert!(round.as_mut().poll(cx).is_pending());
        if reached.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }));
    drop(round);
    clear(&device);
    assert!(device.acquire_snapshot_lease(state(&[1])).is_err());
    assert_eq!(device.retirements.lock().expect("queue").len(), 1);
    assert!(path(&device, 1).exists());
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("retry")
            .completed,
        1
    );
}

#[test]
fn overlapping_round_waits_asynchronously_and_observes_completed_queue() {
    use std::future::Future;
    let parent = tempfile::tempdir().expect("parent");
    let device = device(parent.path().join("device"));
    device.enqueue_retirements(vec![large(1)]).expect("enqueue");
    let (reached, release) = install(&device, false);
    let mut first = Box::pin(device.reclaim_retirements());
    block_on(std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        if reached.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }));
    let mut second = Box::pin(device.reclaim_retirements());
    block_on(std::future::poll_fn(|cx| {
        assert!(
            second.as_mut().poll(cx).is_pending(),
            "overlapping round must await qualification"
        );
        Poll::Ready(())
    }));
    release.store(true, Ordering::Release);
    assert_eq!(block_on(first).expect("first round").completed, 1);
    clear(&device);
    assert_eq!(block_on(second).expect("second round").attempted, 0);
}
