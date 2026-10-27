//! @test @api: BlockDeviceBackend::reclaim_retirements
//! @impl: src/reclaim_impl.rs
//! @design-doc: design/features/04-reconcile-retire.feature
use bytes::Bytes;
use futures_lite::future::{block_on, poll_once};
use pi_block_device::{
    AllocationRun, BlockDeviceState, BlockId, LevelVersions, RecordSeq, SegmentVersion,
};
use pi_block_device::{BlockDeviceBackend, LargeBlockId, RetiredObject};
use pi_block_device::{BlockDeviceWriteBatch, BlockWriteChunk, BlockWritePosition};
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig};
use std::sync::Arc;

fn isolated_device(root: std::path::PathBuf) -> Arc<FileBlockDevice> {
    // SAFETY: isolated temporary root, no external participants.
    block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root }) }).expect("create")
}

fn large(raw: u64) -> RetiredObject {
    RetiredObject::LargeBlock {
        id: LargeBlockId::new(raw).expect("id"),
    }
}

fn active_batch(device: &Arc<FileBlockDevice>) -> pi_file_block_device::FileWriteBatch {
    let mut batch = device.begin_write_batch().expect("batch");
    let request = BlockWriteChunk::new(
        BlockId::ordinary(0, SegmentVersion::new(1).unwrap(), 0).unwrap(),
        Bytes::from_static(b"x"),
        BlockWritePosition::new(0, 1).unwrap(),
    )
    .unwrap();
    block_on(batch.write(request)).expect("first write owns gate");
    batch
}

#[test]
fn empty_reclaim_is_ready_while_batch_is_alive() {
    let temporary = tempfile::tempdir().expect("root");
    let device = isolated_device(temporary.path().join("device"));
    let batch = active_batch(&device);
    let result = block_on(poll_once(device.reclaim_retirements()));
    assert!(
        result.is_some(),
        "empty reclaim must be Ready without dropping active batch"
    );
    let report = result.expect("Ready").expect("empty round");
    assert_eq!(
        (
            report.attempted,
            report.completed,
            report.guarded,
            report.retryable
        ),
        (0, 0, 0, 0)
    );
    drop(batch);
}

#[test]
fn nonempty_reclaim_does_not_wait_for_unused_batch() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("device");
    let device = isolated_device(root.clone());
    let path = root.join("large-000000000000001.bin");
    std::fs::write(&path, b"retired").expect("fixture");
    let unused = device.begin_write_batch().expect("unused batch");
    device.enqueue_retirements(vec![large(1)]).expect("enqueue");
    let report = block_on(device.reclaim_retirements()).expect("unused batch holds no gate");
    assert_eq!(report.completed, 1);
    assert!(!path.exists());
    drop(unused);
}

#[test]
fn nonempty_reclaim_waits_for_batch_and_freezes_selected_objects() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("device");
    let device = isolated_device(root.clone());
    let first = root.join("large-000000000000001.bin");
    let later = root.join("large-000000000000002.bin");
    std::fs::write(&first, b"first").expect("fixture");
    std::fs::write(&later, b"later").expect("fixture");
    device
        .enqueue_retirements(vec![large(1)])
        .expect("enqueue first");
    let batch = active_batch(&device);
    let mut round = Box::pin(device.reclaim_retirements());
    assert!(block_on(poll_once(round.as_mut())).is_none());
    device
        .enqueue_retirements(vec![large(2)])
        .expect("enqueue while waiting");
    assert_eq!(std::fs::read(&first).expect("first retained"), b"first");
    assert_eq!(std::fs::read(&later).expect("later retained"), b"later");
    drop(batch);
    let report = block_on(round).expect("resumed round");
    assert_eq!(
        (
            report.attempted,
            report.completed,
            report.guarded,
            report.retryable
        ),
        (1, 1, 0, 0)
    );
    assert!(!first.exists());
    assert_eq!(std::fs::read(&later).expect("next round only"), b"later");
    let report = block_on(device.reclaim_retirements()).expect("next round");
    assert_eq!(
        (
            report.attempted,
            report.completed,
            report.guarded,
            report.retryable
        ),
        (1, 1, 0, 0)
    );
    assert!(!later.exists());
}

#[test]
fn cancelling_reclaim_write_gate_wait_releases_round_and_keeps_responsibility() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("device");
    let device = isolated_device(root.clone());
    let path = root.join("large-000000000000001.bin");
    std::fs::write(&path, b"retained").expect("fixture");
    device.enqueue_retirements(vec![large(1)]).expect("enqueue");
    let batch = active_batch(&device);
    let mut cancelled = Box::pin(device.reclaim_retirements());
    assert!(block_on(poll_once(cancelled.as_mut())).is_none());
    drop(cancelled);
    let mut replacement = Box::pin(device.reclaim_retirements());
    assert!(block_on(poll_once(replacement.as_mut())).is_none());
    assert_eq!(
        std::fs::read(&path).expect("cancel preserves bytes"),
        b"retained"
    );
    drop(batch);
    let report = block_on(replacement).expect("round gate released and responsibility retained");
    assert_eq!(
        (
            report.attempted,
            report.completed,
            report.guarded,
            report.retryable
        ),
        (1, 1, 0, 0)
    );
    assert!(!path.exists());
}

#[test]
fn public_reclaim_completes_missing_and_following_deduplicated_objects() {
    let temporary = tempfile::tempdir().expect("isolated root");
    let root = temporary.path().join("device");
    // SAFETY: isolated temporary root, fixture writes precede operations.
    let device: Arc<FileBlockDevice> =
        block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root: root.clone() }) })
            .expect("create");
    let first = RetiredObject::LargeBlock {
        id: LargeBlockId::new(1).expect("id"),
    };
    let second = RetiredObject::LargeBlock {
        id: LargeBlockId::new(2).expect("id"),
    };
    let path = root.join("large-000000000000002.bin");
    std::fs::write(&path, b"following").expect("fixture");
    device
        .enqueue_retirements(vec![first, second, second])
        .expect("enqueue");
    let report = block_on(device.reclaim_retirements()).expect("finite round");
    assert_eq!(report.attempted, 2);
    assert_eq!(report.completed, 2);
    assert_eq!(report.guarded + report.retryable, 0);
    assert!(!path.exists());
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("empty")
            .attempted,
        0
    );
}

#[test]
fn sealed_segment_mapping_buffers_guard_single_segment_file() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("sealed");
    // SAFETY: isolated root, fixture writes precede mapping; no bypass writers.
    let device =
        block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root: root.clone() }) })
            .expect("create");
    let version = SegmentVersion::new(1).expect("version");
    let mut state = BlockDeviceState::initial().expect("state");
    let mut data = state.levels[0]
        .versions
        .get(0)
        .expect("active")
        .clone()
        .expect("data");
    data.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: RecordSeq::new(1).expect("seq"),
        len: 2,
    });
    data.released_at.push_back_mut(RecordSeq::NULL);
    data.released_at.push_back_mut(RecordSeq::NULL);
    state.levels[0].versions.set_mut(0, Some(data));
    state.levels[0].versions.push_back_mut(
        LevelVersions::initial(SegmentVersion::new(2).expect("version"))
            .versions
            .get(0)
            .expect("empty")
            .clone(),
    );
    let path = root.join("segment-00-00000001.bin");
    std::fs::write(&path, [vec![1_u8; 4096], vec![2_u8; 4096]].concat()).expect("fixture");
    let lease = device
        .acquire_snapshot_lease(Arc::new(state))
        .expect("lease");
    let first = block_on(device.read(&lease, BlockId::ordinary(0, version, 0).expect("slot")))
        .expect("sealed slot 0");
    let second = block_on(device.read(&lease, BlockId::ordinary(0, version, 1).expect("slot")))
        .expect("sealed slot 1");
    drop(lease);
    device
        .enqueue_retirements(vec![RetiredObject::Segment { level: 0, version }])
        .expect("enqueue");
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("guarded")
            .guarded,
        1
    );
    drop(first);
    assert_eq!(second.as_ref(), vec![2_u8; 4096]);
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("still guarded")
            .guarded,
        1
    );
    drop(second);
    assert_eq!(
        block_on(device.reclaim_retirements())
            .expect("delete single segment")
            .completed,
        1
    );
    assert!(!path.exists());
}
