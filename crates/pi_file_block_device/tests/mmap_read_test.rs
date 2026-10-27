//! @test @api: BlockDeviceBackend::read
//! @impl: src/read_impl.rs
//! @design-doc: design/features/16-architecture-boundaries.feature
use bytes::Bytes;
use futures_lite::future::block_on;
use pi_async_fs::{FileAccessMode, FileFlushMode, FileIo, FileNamespace, LocalFileNamespace};
use pi_block_device::{
    AllocationRun, BlockDeviceBackend, BlockDeviceState, BlockId, RecordSeq, SegmentVersion,
};
use pi_block_device::{BlockDeviceWriteBatch, BlockWriteChunk, BlockWritePosition};
use pi_block_device::{LargeBlockCursor, LargeBlockData, LargeBlockId, ProtectedReadBuf};
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig};
use std::sync::Arc;

#[test]
fn public_read_maps_segment_slot_and_shares_mapping() {
    let temporary = tempfile::tempdir().expect("isolated root");
    let root = temporary.path().join("device");
    // SAFETY: isolated temporary root; fixtures precede mapping, no external participants.
    let device =
        block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root: root.clone() }) })
            .expect("create");
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
    std::fs::write(
        root.join("segment-00-00000001.bin"),
        [vec![3_u8; 4096], vec![7_u8; 4096]].concat(),
    )
    .expect("fixture before mapping");
    let lease = device
        .acquire_snapshot_lease(Arc::new(state))
        .expect("lease");
    let block = BlockId::ordinary(0, SegmentVersion::new(1).expect("version"), 1).expect("slot");
    let result = block_on(device.read(&lease, block));
    assert!(
        result.is_ok(),
        "segment slot read failed: {:?}",
        result.err()
    );
    let first = block_on(device.read(&lease, block)).expect("first");
    let second = block_on(device.read(&lease, block)).expect("shared");
    assert_eq!(first.as_ref(), vec![7_u8; 4096]);
    assert_eq!(first.as_ref().as_ptr(), second.as_ref().as_ptr());
}

fn isolated_device(root: std::path::PathBuf) -> Arc<FileBlockDevice> {
    // SAFETY: tests own isolated temporary roots and perform no uncoordinated
    // mutations during any mapping's lifetime. Fixture writes precede reads.
    block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root }) }).expect("create")
}

fn large_state(len: u64) -> Arc<BlockDeviceState> {
    let mut state = BlockDeviceState::initial().expect("state");
    state.large_blocks.next_id = LargeBlockCursor::new(2).expect("cursor");
    state.large_blocks.entries.insert_mut(
        LargeBlockId::new(1).expect("id"),
        LargeBlockData {
            allocated_at: RecordSeq::new(1).expect("seq"),
            released_at: RecordSeq::NULL,
            logical_len: len,
        },
    );
    Arc::new(state)
}

fn ordinary_state(slots: u32) -> Arc<BlockDeviceState> {
    let mut state = BlockDeviceState::initial().expect("state");
    let mut data = state.levels[0]
        .versions
        .get(0)
        .expect("active")
        .clone()
        .expect("data");
    data.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: RecordSeq::new(1).expect("seq"),
        len: slots,
    });
    for _ in 0..slots {
        data.released_at.push_back_mut(RecordSeq::NULL);
    }
    state.levels[0].versions.set_mut(0, Some(data));
    Arc::new(state)
}

#[test]
fn public_large_read_exceeds_one_mib_and_holds_lease_and_real_mapping() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("large");
    let device = isolated_device(root.clone());
    let len = (1 << 20) + 17;
    let path = root.join("large-000000000000001.bin");
    std::fs::write(&path, vec![9_u8; len]).expect("fixture");
    let state = large_state(len as u64);
    let weak = Arc::downgrade(&state);
    let lease = device.acquire_snapshot_lease(state).expect("lease");
    let buffer = block_on(device.read(&lease, BlockId::large(LargeBlockId::new(1).expect("id"))))
        .expect("large mmap");
    drop(lease);
    assert!(weak.upgrade().is_some());
    let _ = buffer.lease();
    assert_eq!(buffer.as_ref(), vec![9_u8; len]);
    let namespace = LocalFileNamespace::new();
    let mut append =
        block_on(namespace.open(&path, FileAccessMode::Append)).expect("append handle");
    block_on(append.flush(FileFlushMode::DataAndMetadata))
        .expect("metadata flush while large mapping lives");
    block_on(append.flush(FileFlushMode::Data)).expect("data flush while large mapping lives");
    assert_eq!(buffer.as_ref(), vec![9_u8; len]);
    drop(buffer);
    assert!(weak.upgrade().is_none());
    block_on(append.flush(FileFlushMode::DataAndMetadata)).expect("weak cache releases mapping");
}

#[test]
fn active_old_prefix_survives_append_and_new_slot_mapping() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("active");
    let device = isolated_device(root.clone());
    let path = root.join("segment-00-00000001.bin");
    std::fs::write(&path, vec![4_u8; 4096]).expect("prefix");
    let lease = device
        .acquire_snapshot_lease(ordinary_state(1))
        .expect("lease");
    let version = SegmentVersion::new(1).expect("version");
    let first = block_on(device.read(&lease, BlockId::ordinary(0, version, 0).expect("slot")))
        .expect("prefix mmap");
    let namespace = LocalFileNamespace::new();
    let mut append = block_on(namespace.open(&path, FileAccessMode::Append)).expect("append");
    block_on(append.append(Bytes::from(vec![8_u8; 4096]))).expect("strict append");
    block_on(append.flush(FileFlushMode::DataAndMetadata))
        .expect("completed append metadata flush while old mapping lives");
    assert_eq!(first.as_ref(), vec![4_u8; 4096]);
    let new_lease = device
        .acquire_snapshot_lease(ordinary_state(2))
        .expect("new frozen view");
    let second = block_on(device.read(&new_lease, BlockId::ordinary(0, version, 1).expect("slot")))
        .expect("new nonoverlapping mmap");
    assert_eq!(first.as_ref(), vec![4_u8; 4096]);
    assert_eq!(second.as_ref(), vec![8_u8; 4096]);
    block_on(append.flush(FileFlushMode::Data)).expect("data flush while both mappings live");
    block_on(append.flush(FileFlushMode::DataAndMetadata))
        .expect("metadata flush while both mappings live");
    block_on(append.append(Bytes::from(vec![12_u8; 4096])))
        .expect("continued append while both mappings live");
    block_on(append.flush(FileFlushMode::Data)).expect("continued append data flush");
    block_on(append.flush(FileFlushMode::DataAndMetadata))
        .expect("continued append metadata flush");
    let latest_lease = device
        .acquire_snapshot_lease(ordinary_state(3))
        .expect("latest frozen view");
    let third = block_on(device.read(
        &latest_lease,
        BlockId::ordinary(0, version, 2).expect("slot"),
    ))
    .expect("continued nonoverlapping mmap");
    assert_eq!(first.as_ref(), vec![4_u8; 4096]);
    assert_eq!(second.as_ref(), vec![8_u8; 4096]);
    assert_eq!(third.as_ref(), vec![12_u8; 4096]);
}

#[test]
fn large_physical_length_mismatch_is_structured_read_error() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("short");
    let device = isolated_device(root.clone());
    std::fs::write(root.join("large-000000000000001.bin"), vec![0_u8; 16]).expect("fixture");
    let lease = device
        .acquire_snapshot_lease(large_state(17))
        .expect("lease");
    let result = block_on(device.read(&lease, BlockId::large(LargeBlockId::new(1).expect("id"))));
    let error = result.err().expect("length mismatch");
    assert_eq!(error.operation, pi_block_device::DeviceOperation::Read);
    assert_eq!(
        error
            .source
            .downcast_ref::<std::io::Error>()
            .expect("typed")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn unaddressable_large_length_is_rejected_before_file_io() {
    let temporary = tempfile::tempdir().expect("root");
    let device = isolated_device(temporary.path().join("overflow"));
    let lease = device
        .acquire_snapshot_lease(large_state(isize::MAX as u64 + 1))
        .expect("lease");
    let error = block_on(device.read(&lease, BlockId::large(LargeBlockId::new(1).expect("id"))))
        .err()
        .expect("overflow");
    assert_eq!(
        error
            .source
            .downcast_ref::<std::io::Error>()
            .expect("typed")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn concurrent_public_reads_share_one_mapping() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("concurrent");
    let device = isolated_device(root.clone());
    std::fs::write(root.join("large-000000000000001.bin"), vec![6_u8; 4096]).expect("fixture");
    let lease = device
        .acquire_snapshot_lease(large_state(4096))
        .expect("lease");
    let block = BlockId::large(LargeBlockId::new(1).expect("id"));
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| block_on(device.read(&lease, block)).expect("first"));
        let second = scope.spawn(|| block_on(device.read(&lease, block)).expect("second"));
        (
            first.join().expect("thread"),
            second.join().expect("thread"),
        )
    });
    assert_eq!(first.as_ref().as_ptr(), second.as_ref().as_ptr());
    assert_eq!(first.as_ref(), vec![6_u8; 4096]);
}

#[test]
fn incomplete_ordinary_slot_is_rejected_without_mapping_future_bytes() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("short-slot");
    let device = isolated_device(root.clone());
    std::fs::write(root.join("segment-00-00000001.bin"), vec![1_u8; 4097]).expect("fixture");
    let lease = device
        .acquire_snapshot_lease(ordinary_state(2))
        .expect("lease");
    let block = BlockId::ordinary(0, SegmentVersion::new(1).expect("version"), 1).expect("slot");
    let error = block_on(device.read(&lease, block))
        .err()
        .expect("short slot");
    assert_eq!(
        error
            .source
            .downcast_ref::<std::io::Error>()
            .expect("typed")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn public_batch_appends_and_flushes_with_old_mapped_prefix_alive() {
    let temporary = tempfile::tempdir().expect("root");
    let root = temporary.path().join("batch-prefix");
    let device = isolated_device(root.clone());
    let version = SegmentVersion::new(1).expect("version");
    let first_block = BlockId::ordinary(0, version, 0).expect("slot");
    let second_block = BlockId::ordinary(0, version, 1).expect("slot");
    let mut batch = device.begin_write_batch().expect("begin");
    let request = BlockWriteChunk::new(
        first_block,
        Bytes::from(vec![4; 4096]),
        BlockWritePosition::new(0, 4096).expect("position"),
    )
    .expect("chunk");
    block_on(batch.write(request)).expect("first write");
    block_on(batch.prepare()).expect("first batch synced");
    let lease = device
        .acquire_snapshot_lease(ordinary_state(1))
        .expect("lease");
    let first = block_on(device.read(&lease, first_block)).expect("old prefix");
    let mut batch = device.begin_write_batch().expect("next batch");
    let request = BlockWriteChunk::new(
        second_block,
        Bytes::from(vec![8; 4096]),
        BlockWritePosition::new(0, 4096).expect("position"),
    )
    .expect("chunk");
    block_on(batch.write(request)).expect("append with mapping");
    block_on(batch.prepare()).expect("append synced with old mapping alive");
    let lease = device
        .acquire_snapshot_lease(ordinary_state(2))
        .expect("new lease");
    let second = block_on(device.read(&lease, second_block)).expect("new immutable range");
    let shared = block_on(device.read(&lease, second_block)).expect("shared mapping");
    assert_eq!(first.as_ref(), vec![4; 4096]);
    assert_eq!(second.as_ref(), vec![8; 4096]);
    assert_eq!(second.as_ref().as_ptr(), shared.as_ref().as_ptr());
}
