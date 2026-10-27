use super::*;

const FOREIGN_MANIFEST: &[u8] = b"manifest owned by another participant";
static INJECT_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

pub(super) fn after_root_created(root: &Path) {
    let mut target = INJECT_STAGING_ROOT.lock().expect("injection lock");
    if target.as_deref() == Some(root) {
        std::fs::write(root.join(".manifest.staging"), FOREIGN_MANIFEST).expect("foreign staging");
        *target = None;
    }
}

pub(super) fn before_manifest_publish(root: &Path) {
    let mut target = INJECT_ROOT.lock().expect("injection lock");
    if target.as_deref() == Some(root) {
        std::fs::write(manifest_path(root), FOREIGN_MANIFEST).expect("foreign manifest");
        *target = None;
    }
}

#[test]
fn create_collision_preserves_foreign_manifest_and_root() {
    let parent = tempfile::tempdir().expect("test parent");
    let root = parent.path().join("device");
    let sibling = parent.path().join("external.bin");
    std::fs::write(&sibling, b"external parent content").expect("sibling");
    *INJECT_ROOT.lock().expect("injection lock") = Some(root.clone());

    let result =
        futures_lite::future::block_on(test_create(FileBlockDeviceConfig { root: root.clone() }));
    assert!(INJECT_ROOT.lock().expect("injection lock").is_none());
    let error = result
        .err()
        .expect("no-replace publish must fail on foreign manifest");
    assert!(
        error.to_string().contains("already exists"),
        "unexpected create error: {error:?}"
    );
    let actual = std::fs::read(manifest_path(&root));
    assert!(
        matches!(&actual, Ok(bytes) if bytes == FOREIGN_MANIFEST),
        "foreign manifest must remain byte-for-byte unchanged: {actual:?}; root_exists={}",
        root.is_dir()
    );
    assert!(root.is_dir(), "foreign manifest's root must remain");
    assert_eq!(
        std::fs::read(sibling).expect("sibling retained"),
        b"external parent content"
    );
    let retained = error
        .source()
        .and_then(|source| source.downcast_ref::<create_error::PublishError>())
        .expect("structured RenameFailure must be retained");
    assert!(matches!(
        retained.failure.commit_evidence(),
        pi_async_fs::RenameCommitEvidence::NotRenamedByOperation
    ));
    assert_eq!(
        retained.failure.error().current_context().to_string(),
        "already exists"
    );
}

static INJECT_STAGING_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

#[test]
fn create_collision_preserves_foreign_staging_and_creation_evidence() {
    let parent = tempfile::tempdir().expect("test parent");
    let root = parent.path().join("device");
    *INJECT_STAGING_ROOT.lock().expect("injection lock") = Some(root.clone());
    let error =
        futures_lite::future::block_on(test_create(FileBlockDeviceConfig { root: root.clone() }))
            .err()
            .expect("foreign staging must prevent create_new");
    assert!(
        INJECT_STAGING_ROOT
            .lock()
            .expect("injection lock")
            .is_none()
    );
    assert_eq!(
        std::fs::read(root.join(".manifest.staging")).expect("staging retained"),
        FOREIGN_MANIFEST
    );
    assert!(root.is_dir());
    assert!(!manifest_path(&root).exists());
    let retained = error
        .source()
        .and_then(|source| source.downcast_ref::<create_error::CreateError>())
        .expect("structured CreateFailure retained");
    assert!(matches!(
        retained.failure.target_evidence(),
        CreateTargetEvidence::NotCreatedByOperation
    ));
    assert_eq!(
        retained.failure.error().current_context().to_string(),
        "already exists"
    );
}

// SAFETY: all callers use isolated temporary roots. Collision fixtures affect
// only manifest publication; no mapping exists during their injection.
fn test_create(
    config: FileBlockDeviceConfig,
) -> impl Future<Output = Result<Arc<FileBlockDevice>, FileDeviceError>> + Send {
    unsafe { FileBlockDevice::create(config) }
}
