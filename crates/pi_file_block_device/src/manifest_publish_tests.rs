use super::*;
use std::future::{Future, poll_fn};
use std::sync::atomic::Ordering;
use std::task::Poll;

static PAUSE_ROOT: Mutex<Option<(PathBuf, Arc<AtomicBool>)>> = Mutex::new(None);

pub(super) async fn before_flush(root: &Path) {
    let reached = {
        let target = PAUSE_ROOT.lock().expect("pause lock");
        target
            .as_ref()
            .and_then(|(path, reached)| (path == root).then(|| Arc::clone(reached)))
    };
    if let Some(reached) = reached {
        reached.store(true, Ordering::Release);
        std::future::pending::<()>().await;
    }
}

#[test]
fn cancelled_before_flush_is_not_openable() {
    let parent = tempfile::tempdir().expect("test parent");
    let root = parent.path().join("device");
    let config = FileBlockDeviceConfig { root: root.clone() };
    let reached = Arc::new(AtomicBool::new(false));
    *PAUSE_ROOT.lock().expect("pause lock") = Some((root.clone(), Arc::clone(&reached)));
    let mut create = Box::pin(test_create(config.clone()));
    futures_lite::future::block_on(poll_fn(|cx| {
        assert!(
            create.as_mut().poll(cx).is_pending(),
            "create must pause before flush"
        );
        if reached.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }));
    drop(create);
    *PAUSE_ROOT.lock().expect("pause lock") = None;
    let opened = futures_lite::future::block_on(test_open(config, None));
    assert!(
        matches!(opened, Err(FileDeviceError::NotFound)),
        "cancelled create before flush must not be accepted as initialized"
    );
    assert!(!manifest_path(&root).exists());
    assert_eq!(
        std::fs::read(root.join(".manifest.staging"))
            .expect("orphan staging retained")
            .len(),
        MANIFEST_LEN
    );
    assert!(matches!(
        futures_lite::future::block_on(test_create(FileBlockDeviceConfig { root })),
        Err(FileDeviceError::AlreadyExists)
    ));
}

#[test]
fn successful_create_reopens_same_identity_without_staging() {
    let parent = tempfile::tempdir().expect("test parent");
    let root = parent.path().join("device");
    let config = FileBlockDeviceConfig { root: root.clone() };
    let created = futures_lite::future::block_on(test_create(config.clone())).expect("create");
    let opened =
        futures_lite::future::block_on(test_open(config.clone(), Some(created.device_id())))
            .expect("open published manifest");
    assert_eq!(opened.device_id(), created.device_id());
    assert_eq!(
        std::fs::read(manifest_path(&root)).expect("manifest").len(),
        MANIFEST_LEN
    );
    assert!(!root.join(".manifest.staging").exists());
    assert!(matches!(
        futures_lite::future::block_on(test_create(config)),
        Err(FileDeviceError::AlreadyExists)
    ));
    let reopened = futures_lite::future::block_on(test_open(
        FileBlockDeviceConfig { root },
        Some(created.device_id()),
    ))
    .expect("existing device unchanged");
    assert_eq!(reopened.device_id(), created.device_id());
}

// SAFETY: all callers use isolated temporary roots. Collision fixtures affect
// only manifest publication; no mapping exists during their injection.
fn test_create(
    config: FileBlockDeviceConfig,
) -> impl Future<Output = Result<Arc<FileBlockDevice>, FileDeviceError>> + Send {
    unsafe { FileBlockDevice::create(config) }
}

fn test_open(
    config: FileBlockDeviceConfig,
    expected: Option<BlockDeviceId>,
) -> impl Future<Output = Result<Arc<FileBlockDevice>, FileDeviceError>> + Send {
    // SAFETY: isolated test roots; no uncoordinated access during mapped reads.
    unsafe { FileBlockDevice::open(config, expected) }
}
