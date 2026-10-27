//! @test @api: public write/prepare/open/reconcile/read with real Local I/O
//! @impl: src/write_impl.rs, src/reconcile_retire.rs
//! @design-doc: design/features/03-backend-session.feature, design/features/04-reconcile-retire.feature
use bytes::Bytes;
use futures_lite::future::block_on;
use pi_block_device::{
    AllocationRun, BlockDeviceBackend, BlockDeviceState, BlockDeviceWriteBatch, BlockId,
    BlockWriteChunk, BlockWritePosition, LargeBlockCursor, LargeBlockData, LargeBlockId,
    ReconcileFailureKind, RecordSeq, SegmentVersion,
};
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig};
use std::sync::Arc;

fn state() -> BlockDeviceState {
    let mut state = BlockDeviceState::initial().expect("state");
    let mut data = state.levels[0].versions.get(0).unwrap().clone().unwrap();
    data.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: RecordSeq::new(1).unwrap(),
        len: 1,
    });
    data.released_at.push_back_mut(RecordSeq::NULL);
    state.levels[0].versions.set_mut(0, Some(data));
    assert!(state.validate());
    state
}
fn slot(index: u32) -> BlockId {
    BlockId::ordinary(0, SegmentVersion::new(1).unwrap(), index).unwrap()
}
fn chunk(block: BlockId, offset: u64, len: u64, bytes: Vec<u8>) -> BlockWriteChunk {
    BlockWriteChunk::new(
        block,
        Bytes::from(bytes),
        BlockWritePosition::new(offset, len).unwrap(),
    )
    .unwrap()
}

#[test]
fn prepared_chunks_reopen_same_identity_and_read_exact_payload_and_padding() {
    let temp = tempfile::tempdir().unwrap();
    let config = FileBlockDeviceConfig {
        root: temp.path().join("device"),
    };
    // SAFETY: isolated root, coordinated I/O, mappings released before cleanup.
    let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
    let id = device.device_id();
    let large = LargeBlockId::new(1).unwrap();
    let len = (1 << 20) + 17;
    let mut batch = device.begin_write_batch().unwrap();
    block_on(batch.write(chunk(slot(0), 0, 6, vec![3; 2]))).unwrap();
    block_on(batch.write(chunk(BlockId::large(large), 0, len, vec![9; 1 << 20]))).unwrap();
    block_on(batch.write(chunk(slot(0), 2, 6, vec![7; 4]))).unwrap();
    block_on(batch.write(chunk(BlockId::large(large), 1 << 20, len, vec![8; 17]))).unwrap();
    block_on(batch.prepare()).expect("real completed sync");
    drop(device);
    let mut expected = state();
    expected.large_blocks.next_id = LargeBlockCursor::new(2).unwrap();
    expected.large_blocks.entries.insert_mut(
        large,
        LargeBlockData {
            allocated_at: RecordSeq::new(1).unwrap(),
            released_at: RecordSeq::NULL,
            logical_len: len,
        },
    );
    assert!(expected.validate());
    // SAFETY: prior handles released, no uncoordinated participants.
    let device = block_on(unsafe { FileBlockDevice::open(config, Some(id)) }).unwrap();
    assert_eq!(device.device_id(), id);
    let expected = Arc::new(expected);
    let report = block_on(device.reconcile(Arc::clone(&expected))).expect("recovered");
    assert!(report.progress.media_metadata_synced);
    let lease = device.acquire_snapshot_lease(expected).unwrap();
    let ordinary = block_on(device.read(&lease, slot(0))).unwrap();
    let mut bytes = vec![0; 4096];
    bytes[..6].copy_from_slice(&[3, 3, 7, 7, 7, 7]);
    assert_eq!(ordinary.as_ref(), bytes);
    let large = block_on(device.read(&lease, BlockId::large(large))).unwrap();
    assert_eq!(large.as_ref(), [vec![9; 1 << 20], vec![8; 17]].concat());
}

#[test]
fn reopen_repairs_uncommitted_partial_active_tail_and_can_append_again() {
    let temp = tempfile::tempdir().unwrap();
    let config = FileBlockDeviceConfig {
        root: temp.path().join("device"),
    };
    // SAFETY: isolated root, no external participants.
    let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
    let id = device.device_id();
    let mut batch = device.begin_write_batch().unwrap();
    block_on(batch.write(chunk(slot(0), 0, 4096, vec![4; 4096]))).unwrap();
    block_on(batch.prepare()).expect("authoritative prefix synced");
    let mut batch = device.begin_write_batch().unwrap();
    block_on(batch.write(chunk(slot(1), 0, 6, vec![7; 2]))).unwrap();
    drop(batch);
    assert!(device.begin_write_batch().is_err());
    drop(device);
    // SAFETY: reopening after all prior handles released.
    let device = block_on(unsafe { FileBlockDevice::open(config.clone(), Some(id)) }).unwrap();
    let report = block_on(device.reconcile(Arc::new(state()))).expect("repair partial tail");
    assert_eq!(report.progress.planned_actions, 1);
    assert_eq!(report.progress.completed_actions, 1);
    assert_eq!(report.progress.remaining_actions, 0);
    assert!(!report.progress.partial_progress);
    assert!(report.progress.media_metadata_synced);
    assert_eq!(
        std::fs::read(config.root.join("segment-00-00000001.bin")).unwrap(),
        vec![4; 4096]
    );
    let repeat = block_on(device.reconcile(Arc::new(state()))).unwrap();
    assert_eq!(repeat.progress.planned_actions, 0);
    let lease = device.acquire_snapshot_lease(Arc::new(state())).unwrap();
    let prefix = block_on(device.read(&lease, slot(0))).unwrap();
    let mut batch = device.begin_write_batch().unwrap();
    block_on(batch.write(chunk(slot(1), 0, 1, vec![8]))).unwrap();
    block_on(batch.prepare()).unwrap();
    assert_eq!(prefix.as_ref(), vec![4; 4096]);
}

#[test]
fn recovery_sync_failure_keeps_source_and_does_not_claim_success() {
    let temp = tempfile::tempdir().unwrap();
    let config = FileBlockDeviceConfig {
        root: temp.path().join("device"),
    };
    // SAFETY: isolated fixture replacement before any mapping or operation.
    let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
    let manifest = config.root.join("manifest.bin");
    std::fs::remove_file(&manifest).unwrap();
    std::fs::create_dir(&manifest).unwrap();
    let error = block_on(device.reconcile(Arc::new(BlockDeviceState::initial().unwrap())))
        .expect_err("actual manifest sync handle failure");
    assert_eq!(error.kind, ReconcileFailureKind::MediaMetadataSync);
    assert!(error.source.is_some());
    assert!(format!("{:?}", error.source).contains("IoFailure"));
    assert!(!error.progress.media_metadata_synced);
    assert_eq!(error.progress.completed_actions, 0);
    assert!(device.begin_write_batch().is_err());
}

#[test]
fn sealed_and_large_excess_lengths_are_corrupt_and_never_repaired() {
    for large in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let config = FileBlockDeviceConfig {
            root: temp.path().join("device"),
        };
        // SAFETY: isolated fixtures before mapping or recovery.
        let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
        let mut expected = state();
        let path = if large {
            expected = BlockDeviceState::initial().unwrap();
            expected.large_blocks.next_id = LargeBlockCursor::new(2).unwrap();
            expected.large_blocks.entries.insert_mut(
                LargeBlockId::new(1).unwrap(),
                LargeBlockData {
                    allocated_at: RecordSeq::new(1).unwrap(),
                    released_at: RecordSeq::NULL,
                    logical_len: 4096,
                },
            );
            config.root.join("large-000000000000001.bin")
        } else {
            let empty = BlockDeviceState::initial().unwrap();
            expected.levels[0]
                .versions
                .push_back_mut(empty.levels[0].versions.get(0).unwrap().clone());
            config.root.join("segment-00-00000001.bin")
        };
        assert!(expected.validate());
        std::fs::write(&path, vec![5; 4098]).unwrap();
        let error = block_on(device.reconcile(Arc::new(expected))).expect_err("non-Active excess");
        assert_eq!(error.kind, ReconcileFailureKind::RequiredObjectCorrupt);
        assert!(error.source.is_some());
        assert_eq!(std::fs::read(path).unwrap(), vec![5; 4098]);
    }
}

#[test]
fn tail_repair_refuses_existing_lease_and_mapping_without_truncating() {
    let temp = tempfile::tempdir().unwrap();
    let config = FileBlockDeviceConfig {
        root: temp.path().join("device"),
    };
    // SAFETY: fixture mutation before mapping.
    let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
    let path = config.root.join("segment-00-00000001.bin");
    std::fs::write(&path, [vec![4; 4096], vec![7; 2]].concat()).unwrap();
    let lease = device.acquire_snapshot_lease(Arc::new(state())).unwrap();
    let prefix = block_on(device.read(&lease, slot(0))).unwrap();
    let error = block_on(device.reconcile(Arc::new(state()))).expect_err("protected tail");
    assert_eq!(error.kind, ReconcileFailureKind::ConcurrentMutation);
    assert!(error.source.is_some());
    assert_eq!(std::fs::metadata(path).unwrap().len(), 4098);
    assert_eq!(prefix.as_ref(), vec![4; 4096]);
}

fn cleaned_virtual_active_can_materialize(residue: &[u8]) {
    let temp = tempfile::tempdir().unwrap();
    let config = FileBlockDeviceConfig {
        root: temp.path().join("device"),
    };
    // SAFETY: isolated fixture before any mapping, all later I/O coordinated.
    let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
    let path = config.root.join("segment-00-00000001.bin");
    std::fs::write(&path, residue).unwrap();
    let expected = Arc::new(BlockDeviceState::initial().unwrap());
    let report = block_on(device.reconcile(Arc::clone(&expected))).unwrap();
    assert_eq!(report.progress.planned_actions, 1);
    assert_eq!(report.progress.completed_actions, 1);
    assert!(report.progress.media_metadata_synced);
    assert!(!path.exists());
    let repeat = block_on(device.reconcile(expected)).unwrap();
    assert_eq!(repeat.progress.planned_actions, 0);
    assert!(repeat.progress.media_metadata_synced);
    let mut batch = device.begin_write_batch().unwrap();
    block_on(batch.write(chunk(slot(0), 0, 6, vec![3; 2])))
        .expect("cleaned virtual Active must allow same identity slot zero");
    block_on(batch.write(chunk(slot(0), 2, 6, vec![7; 4]))).unwrap();
    block_on(batch.prepare()).unwrap();
    let lease = device.acquire_snapshot_lease(Arc::new(state())).unwrap();
    let bytes = block_on(device.read(&lease, slot(0))).unwrap();
    let mut expected = vec![0; 4096];
    expected[..6].copy_from_slice(&[3, 3, 7, 7, 7, 7]);
    assert_eq!(bytes.as_ref(), expected);
}

#[test]
fn zero_length_virtual_active_cleanup_allows_first_materialization() {
    cleaned_virtual_active_can_materialize(&[]);
}

#[test]
fn partial_first_slot_cleanup_allows_first_materialization() {
    cleaned_virtual_active_can_materialize(&[9, 9]);
}

#[test]
fn retired_segment_tombstone_is_not_released_by_virtual_active_reconcile() {
    let temp = tempfile::tempdir().unwrap();
    let config = FileBlockDeviceConfig {
        root: temp.path().join("device"),
    };
    // SAFETY: isolated fixture, no mapping or outside participants.
    let device = block_on(unsafe { FileBlockDevice::create(config.clone()) }).unwrap();
    let path = config.root.join("segment-00-00000001.bin");
    std::fs::write(&path, vec![4; 4096]).unwrap();
    device
        .enqueue_retirements(vec![pi_block_device::RetiredObject::Segment {
            level: 0,
            version: SegmentVersion::new(1).unwrap(),
        }])
        .unwrap();
    assert_eq!(block_on(device.reclaim_retirements()).unwrap().completed, 1);
    assert!(!path.exists());
    block_on(device.reconcile(Arc::new(BlockDeviceState::initial().unwrap()))).unwrap();
    assert!(device.acquire_snapshot_lease(Arc::new(state())).is_err());
    let mut batch = device.begin_write_batch().unwrap();
    assert!(block_on(batch.write(chunk(slot(0), 0, 1, vec![8]))).is_err());
    assert!(!path.exists());
}
