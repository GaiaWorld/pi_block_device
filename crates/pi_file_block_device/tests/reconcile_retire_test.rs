//! @test @api: BlockDeviceBackend::reconcile
//! @impl: src/reconcile_retire.rs
use futures_lite::future::block_on;
use pi_block_device::{
    AllocationRun, BackendCapabilities, BlockDeviceBackend, BlockDeviceState, ReconcileFailureKind,
    RecordSeq,
};
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig};
use std::sync::Arc;

fn create(root: std::path::PathBuf) -> Arc<FileBlockDevice> {
    // SAFETY: isolated temporary roots; fixture mutations precede all mappings.
    block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root }) }).expect("create")
}
fn state(level: usize) -> Arc<BlockDeviceState> {
    let mut state = BlockDeviceState::initial().expect("state");
    let mut data = state.levels[level]
        .versions
        .get(0)
        .expect("active")
        .clone()
        .expect("data");
    data.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: RecordSeq::new(1).expect("seq"),
        len: 1,
    });
    data.released_at.push_back_mut(RecordSeq::NULL);
    state.levels[level].versions.set_mut(0, Some(data));
    Arc::new(state)
}
#[test]
fn reconcile_validates_segment_length_and_syncs_required_files() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("device");
    let device = create(root.clone());
    let missing = block_on(device.reconcile(state(1))).expect_err("missing");
    assert_eq!(missing.kind, ReconcileFailureKind::RequiredObjectMissing);
    assert!(missing.source.is_some());
    let path = root.join("segment-01-00000001.bin");
    std::fs::write(
        &path,
        vec![0_u8; BackendCapabilities::fixed().level_sizes[1] as usize],
    )
    .expect("fixture");
    let report = block_on(device.reconcile(state(1))).expect("required files synced");
    assert!(report.progress.media_metadata_synced);
    assert_eq!(report.progress.planned_actions, 0);
    assert!(!report.progress.partial_progress);
    std::fs::write(&path, vec![0_u8; 4096]).expect("short fixture");
    let corrupt = block_on(device.reconcile(state(1))).expect_err("corrupt");
    assert_eq!(corrupt.kind, ReconcileFailureKind::RequiredObjectCorrupt);
    assert!(corrupt.source.is_some());
}
#[test]
fn reconcile_removes_only_managed_temporary_objects_and_reports_success() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("device");
    let device = create(root.clone());
    let managed = root.join(".write-00000000000000af.tmp");
    let unrelated = root.join(".write-not-managed.tmp");
    std::fs::write(&managed, b"partial").expect("fixture");
    std::fs::write(&unrelated, b"keep").expect("fixture");
    let report = block_on(device.reconcile(Arc::new(BlockDeviceState::initial().expect("state"))))
        .expect("cleanup completed");
    assert_eq!(report.progress.completed_actions, 1);
    assert_eq!(report.progress.planned_actions, 1);
    assert_eq!(report.progress.remaining_actions, 0);
    assert!(report.progress.media_metadata_synced);
    assert!(!report.progress.partial_progress);
    let repeated =
        block_on(device.reconcile(Arc::new(BlockDeviceState::initial().expect("state"))))
            .expect("idempotent");
    assert_eq!(repeated.progress.planned_actions, 0);
    assert!(repeated.progress.media_metadata_synced);
    assert!(!managed.exists());
    assert!(unrelated.exists());
}
#[test]
fn reconcile_does_not_delete_snapshot_protected_orphan() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("device");
    let device = create(root.clone());
    let path = root.join("segment-00-00000001.bin");
    std::fs::write(&path, vec![5_u8; 4096]).expect("fixture");
    let lease = device.acquire_snapshot_lease(state(0)).expect("lease");
    let error = block_on(device.reconcile(Arc::new(BlockDeviceState::initial().expect("state"))))
        .expect_err("protected");
    assert_eq!(error.kind, ReconcileFailureKind::ConcurrentMutation);
    assert!(path.exists());
    drop(lease);
}
