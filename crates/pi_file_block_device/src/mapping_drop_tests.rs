//! @test @api: public read and last ReadBuf drop with real mapping.
//! @impl: src/read_impl.rs
//! @design-doc: design/features/16-architecture-boundaries.feature
use super::*;
use futures_lite::future::block_on;
use pi_block_device::{LargeBlockCursor, LargeBlockData, LargeBlockId, RecordSeq};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::sync::{LazyLock, mpsc};
use std::task::Poll;

struct Hook {
    reached: mpsc::Sender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
    waiting: AtomicBool,
}
static HOOKS: LazyLock<Mutex<BTreeMap<PathBuf, Arc<Hook>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub(super) fn before_unregister(path: &Path) {
    let hook = HOOKS.lock().expect("hooks").get(path).cloned();
    if let Some(hook) = hook {
        hook.reached.send(()).expect("drop reached");
        // Only a separate test drop thread waits here. No production cache or
        // completion lock is held, and the read thread never waits on this channel.
        let _ = hook.resume.lock().expect("resume").recv();
    }
}

pub(super) fn waiting(path: &Path) {
    if let Some(hook) = HOOKS.lock().expect("hooks").get(path) {
        hook.waiting.store(true, Ordering::Release);
    }
}

struct Resume {
    path: PathBuf,
    sender: mpsc::Sender<()>,
}
impl Drop for Resume {
    fn drop(&mut self) {
        HOOKS.lock().expect("hooks").remove(&self.path);
        let _ = self.sender.send(());
    }
}

#[test]
fn public_read_waits_for_last_mapping_drop_to_unregister_range() {
    let temporary = tempfile::tempdir().expect("isolated root");
    let root = temporary.path().join("device");
    // SAFETY: isolated root; fixture writes precede mappings, no bypass mutations.
    let device =
        block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root: root.clone() }) })
            .expect("create");
    let id = LargeBlockId::new(1).expect("id");
    let block = BlockId::large(id);
    let path = device.block_path(block);
    std::fs::write(&path, b"mapped bytes").expect("fixture");
    let mut state = BlockDeviceState::initial().expect("state");
    state.large_blocks.next_id = LargeBlockCursor::new(2).expect("cursor");
    state.large_blocks.entries.insert_mut(
        id,
        LargeBlockData {
            allocated_at: RecordSeq::new(1).expect("seq"),
            released_at: RecordSeq::NULL,
            logical_len: 12,
        },
    );
    let lease = device
        .acquire_snapshot_lease(Arc::new(state))
        .expect("lease");
    let buffer = block_on(device.read(&lease, block)).expect("real mapping");
    assert_eq!(buffer.as_ref(), b"mapped bytes");
    let (reached_tx, reached) = mpsc::channel();
    let (resume_tx, resume) = mpsc::channel();
    let hook = Arc::new(Hook {
        reached: reached_tx,
        resume: Mutex::new(resume),
        waiting: AtomicBool::new(false),
    });
    HOOKS
        .lock()
        .expect("hooks")
        .insert(path.clone(), Arc::clone(&hook));
    std::thread::scope(|scope| {
        let dropping = scope.spawn(move || drop(buffer));
        let release = Resume {
            path,
            sender: resume_tx,
        };
        reached
            .recv()
            .expect("last owner drop paused before unregister");
        let mut read = Box::pin(device.read(&lease, block));
        block_on(std::future::poll_fn(|cx| match read.as_mut().poll(cx) {
            Poll::Ready(result) => panic!(
                "read must await unregister, not complete or report Conflict: {:?}",
                result.err()
            ),
            Poll::Pending if hook.waiting.load(Ordering::Acquire) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }));
        // Cancelling a waiter must not cancel or own the mapping's destruction.
        drop(read);
        let mut read = Box::pin(device.read(&lease, block));
        block_on(std::future::poll_fn(|cx| {
            assert!(read.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }));
        drop(release);
        dropping.join().expect("drop thread");
        let buffer = block_on(read).expect("read succeeds after actual unregister");
        assert_eq!(buffer.as_ref(), b"mapped bytes");
    });
}
