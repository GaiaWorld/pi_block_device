//! @test: manifest public regressions, migrated from legacy write-session tests.
use futures_lite::future::block_on;
use pi_block_device::{
    BackendCapabilities, BlockDeviceBackend, BlockDeviceId, BlockDeviceWriteBatch, ProtectedReadBuf,
};
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig, FileDeviceError};
use std::sync::Arc;
// SAFETY: constructor helpers are only used with isolated temporary roots or
// rejected invalid configurations. No external accesses overlap mapped reads.
const MANIFEST_NAME: &str = "manifest.bin";

fn root(base: &std::path::Path, name: &str) -> std::path::PathBuf {
    base.join(name)
}

fn create_result(root: std::path::PathBuf) -> Result<Arc<FileBlockDevice>, FileDeviceError> {
    block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root }) })
}

fn open_result(
    root: std::path::PathBuf,
    expected: Option<BlockDeviceId>,
) -> Result<Arc<FileBlockDevice>, FileDeviceError> {
    block_on(unsafe { FileBlockDevice::open(FileBlockDeviceConfig { root }, expected) })
}

fn expect_create_err(
    result: Result<Arc<FileBlockDevice>, FileDeviceError>,
    message: &str,
) -> FileDeviceError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("{message}"),
    }
}

fn manifest_bytes(root: &std::path::Path) -> Vec<u8> {
    std::fs::read(root.join(MANIFEST_NAME)).expect("read manifest")
}

/// create 写入固定长度 manifest；open 恢复同一身份且 capabilities 固定。
#[test]
fn fixed_manifest_layout_and_reopen_roundtrip() {
    let temporary = tempfile::tempdir().expect("temp dir");
    let root = root(temporary.path(), "dev-a");
    let device = create_result(root.clone()).expect("create");
    assert!(root.join(MANIFEST_NAME).is_file());
    assert_eq!(manifest_bytes(&root).len(), 8 + 2 + 2 + 16 + 4 + 15 * 4 + 1);

    let reopened = open_result(root.clone(), Some(device.device_id())).expect("open");
    assert_eq!(reopened.device_id(), device.device_id());
    assert_eq!(reopened.capabilities(), BackendCapabilities::fixed());
}

/// 再次 create 返回 AlreadyExists 且既有 manifest 逐字节不变。
#[test]
fn create_rejects_existing_without_mutation() {
    let temporary = tempfile::tempdir().expect("temp dir");
    let root = root(temporary.path(), "dev-b");
    create_result(root.clone()).expect("first create");
    let before = manifest_bytes(&root);
    let error = expect_create_err(create_result(root.clone()), "second create must fail");
    assert!(matches!(error, FileDeviceError::AlreadyExists));
    assert_eq!(manifest_bytes(&root), before);
}

/// 空或相对 root 配置被拒绝为 InvalidConfiguration，零副作用。
#[test]
fn create_open_reject_invalid_configuration() {
    let temporary = tempfile::tempdir().expect("temp dir");
    let error = expect_create_err(
        create_result(std::path::PathBuf::from("")),
        "empty root must be invalid",
    );
    assert!(matches!(error, FileDeviceError::InvalidConfiguration));

    let relative = std::path::PathBuf::from("some/relative/path");
    let error = expect_create_err(
        create_result(relative.clone()),
        "relative root must be invalid",
    );
    assert!(matches!(error, FileDeviceError::InvalidConfiguration));

    let error = expect_create_err(
        open_result(relative.clone(), None),
        "relative root must be invalid for open",
    );
    assert!(matches!(error, FileDeviceError::InvalidConfiguration));
    assert!(!temporary.path().join("some").exists());
}

/// open 不存在的 root 返回 NotFound。
#[test]
fn open_missing_root_is_not_found() {
    let temporary = tempfile::tempdir().expect("temp dir");
    let missing = root(temporary.path(), "never-created");
    let error = expect_create_err(
        open_result(missing.clone(), None),
        "missing root must be NotFound",
    );
    assert!(matches!(error, FileDeviceError::NotFound));
    assert!(!missing.exists());
}

/// 损坏 manifest 被拒绝为 InvalidManifest 且不修复介质。
#[test]
fn open_rejects_corrupt_manifest_without_repair() {
    let temporary = tempfile::tempdir().expect("temp dir");
    let root = root(temporary.path(), "dev-c");
    std::fs::create_dir(&root).expect("create dir");
    let path = root.join(MANIFEST_NAME);
    std::fs::write(&path, b"not-a-manifest").expect("corrupt manifest");
    let error = expect_create_err(
        open_result(root.clone(), None),
        "corrupt manifest must fail",
    );
    assert!(matches!(error, FileDeviceError::InvalidManifest));
    assert_eq!(std::fs::read(&path).expect("unchanged"), b"not-a-manifest");

    std::fs::write(&path, vec![0_u8; 200]).expect("all-zero manifest");
    let error = expect_create_err(
        open_result(root.clone(), None),
        "all-zero manifest must fail",
    );
    assert!(matches!(error, FileDeviceError::InvalidManifest));
}

/// expected_device_id 不匹配返回 IdentityMismatch，介质保持不变。
#[test]
fn open_rejects_identity_mismatch_non_destructively() {
    let temporary = tempfile::tempdir().expect("temp dir");
    let root = root(temporary.path(), "dev-d");
    let device = create_result(root.clone()).expect("create");
    let wrong = BlockDeviceId::generate().expect("generated id");
    assert_ne!(wrong, device.device_id());
    let before = manifest_bytes(&root);
    let error = expect_create_err(open_result(root.clone(), Some(wrong)), "identity mismatch");
    assert!(matches!(error, FileDeviceError::IdentityMismatch));
    assert_eq!(manifest_bytes(&root), before);
}

/// None 与 Some(匹配) 均成功并返回同一身份。
#[test]
fn open_accepts_matching_or_none_expected_identity() {
    let temporary = tempfile::tempdir().expect("temp dir");
    let root = root(temporary.path(), "dev-e");
    let device = create_result(root.clone()).expect("create");
    let none = open_result(root.clone(), None).expect("open with None");
    let matching = open_result(root.clone(), Some(device.device_id())).expect("open with matching");
    assert_eq!(none.device_id(), device.device_id());
    assert_eq!(matching.device_id(), device.device_id());
}
#[test]
fn backend_trait_bounds_hold_for_real_types() {
    fn backend<B: BlockDeviceBackend>() {}
    fn batch<B: BlockDeviceWriteBatch>() {}
    fn buffer<B: ProtectedReadBuf>() {}
    backend::<FileBlockDevice>();
    batch::<<FileBlockDevice as BlockDeviceBackend>::WriteBatch>();
    buffer::<<FileBlockDevice as BlockDeviceBackend>::ReadBuf>();
}
