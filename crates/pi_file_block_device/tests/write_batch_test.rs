//! @test @api: BlockDeviceBackend::begin_write_batch / BlockDeviceWriteBatch
//! @impl: src/write_impl.rs
//! @design-doc: design/features/03-backend-session.feature
use bytes::Bytes;
use futures_lite::future::block_on;
use pi_block_device::{BlockDeviceBackend, BlockDeviceWriteBatch};
use pi_block_device::{BlockId, BlockWriteChunk, BlockWritePosition, SegmentVersion};
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig};

#[test]
fn begin_is_side_effect_free_and_drop_does_not_panic() {
    let temp = tempfile::tempdir().expect("root");
    let root = temp.path().join("device");
    // SAFETY: isolated root with no external participants.
    let device =
        block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root: root.clone() }) })
            .expect("create");
    let result = device.begin_write_batch();
    assert!(result.is_ok(), "begin must be implemented");
    drop(result);
    assert_eq!(std::fs::read_dir(root).expect("dir").count(), 1);
}

fn assert_send<T: Send>(_: T) {}

#[test]
fn unused_batch_does_not_reserve_write_gate() {
    let temp = tempfile::tempdir().expect("root");
    let device = device(temp.path().join("device"));
    let unused = device.begin_write_batch().expect("unused batch");
    let mut second = device
        .begin_write_batch()
        .expect("another synchronous begin");
    block_on(second.write(chunk(slot(0), 0, 1, vec![1])))
        .expect("unused batch must not reserve the gate");
    drop(unused);
}

fn slot(index: u32) -> BlockId {
    BlockId::ordinary(0, SegmentVersion::new(1).expect("version"), index).expect("slot")
}

fn chunk(block: BlockId, offset: u64, len: u64, bytes: Vec<u8>) -> BlockWriteChunk {
    BlockWriteChunk::new(
        block,
        Bytes::from(bytes),
        BlockWritePosition::new(offset, len).expect("position"),
    )
    .expect("chunk")
}

#[test]
fn write_contiguous_chunks_and_pad_only_the_slot_tail() {
    let temp = tempfile::tempdir().expect("root");
    let root = temp.path().join("device");
    // SAFETY: isolated root with no external participants.
    let device =
        block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root: root.clone() }) })
            .expect("create");
    let mut batch = device.begin_write_batch().expect("begin");
    let first = block_on(batch.write(chunk(slot(0), 0, 6, vec![3; 2])));
    assert!(first.is_ok(), "checked first chunk must write: {first:?}");
    block_on(batch.write(chunk(slot(0), 2, 6, vec![7; 4]))).expect("last");
    drop(batch);
    let bytes = std::fs::read(root.join("segment-00-00000001.bin")).expect("object preserved");
    assert_eq!(bytes.len(), 4096);
    assert_eq!(&bytes[..6], &[3, 3, 7, 7, 7, 7]);
    assert!(bytes[6..].iter().all(|b| *b == 0));
}

#[test]
fn batch_futures_are_send() {
    let temp = tempfile::tempdir().expect("root");
    // SAFETY: isolated root with no external participants.
    let device = block_on(unsafe {
        FileBlockDevice::create(FileBlockDeviceConfig {
            root: temp.path().join("device"),
        })
    })
    .expect("create");
    let batch = device.begin_write_batch().expect("begin");
    assert_send(batch.prepare());
    let mut batch = device.begin_write_batch().expect("next begin");
    assert_send(batch.write(chunk(slot(0), 0, 1, vec![1])));
}

#[test]
fn prepare_checks_incomplete_before_namespace_and_preserves_residue() {
    let temp = tempfile::tempdir().expect("root");
    let root = temp.path().join("device");
    // SAFETY: isolated root with no external participants.
    let device =
        block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root: root.clone() }) })
            .expect("create");
    let mut batch = device.begin_write_batch().expect("begin");
    block_on(batch.write(chunk(slot(0), 0, 6, vec![3; 2]))).expect("prefix");
    let error = block_on(batch.prepare()).expect_err("incomplete");
    assert_eq!(
        error
            .source
            .downcast_ref::<std::io::Error>()
            .expect("typed incomplete")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(
        std::fs::read(root.join("segment-00-00000001.bin")).expect("residue"),
        vec![3; 2]
    );
    assert!(device.begin_write_batch().is_err());
}

#[test]
fn complete_prepare_flushes_and_allows_next_batch() {
    let temp = tempfile::tempdir().expect("root");
    let root = temp.path().join("device");
    // SAFETY: isolated root with no external participants.
    let device =
        block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root: root.clone() }) })
            .expect("create");
    let mut batch = device.begin_write_batch().expect("begin");
    block_on(batch.write(chunk(slot(0), 0, 1, vec![9]))).expect("write");
    block_on(batch.prepare()).expect("complete file sync");
    drop(
        device
            .begin_write_batch()
            .expect("not fenced after success"),
    );
    assert_eq!(
        std::fs::metadata(root.join("segment-00-00000001.bin"))
            .expect("preserved")
            .len(),
        4096
    );
}

fn device(root: std::path::PathBuf) -> std::sync::Arc<FileBlockDevice> {
    // SAFETY: isolated test roots; no external participants.
    block_on(unsafe { FileBlockDevice::create(FileBlockDeviceConfig { root }) }).expect("create")
}

#[test]
fn rejects_gap_overlap_duplicate_and_changed_length_without_append() {
    let temp = tempfile::tempdir().expect("root");
    let root = temp.path().join("device");
    let device = device(root.clone());
    let mut batch = device.begin_write_batch().expect("begin");
    assert!(block_on(batch.write(chunk(slot(0), 1, 6, vec![2]))).is_err());
    assert!(!root.join("segment-00-00000001.bin").exists());
    block_on(batch.write(chunk(slot(0), 0, 6, vec![3; 2]))).expect("prefix");
    for (offset, len) in [(0, 6), (1, 6), (3, 6), (2, 7)] {
        assert!(block_on(batch.write(chunk(slot(0), offset, len, vec![8]))).is_err());
    }
    assert_eq!(
        std::fs::read(root.join("segment-00-00000001.bin")).expect("prefix"),
        vec![3; 2]
    );
    block_on(batch.write(chunk(slot(0), 2, 6, vec![7; 4]))).expect("correct continuation");
    assert!(block_on(batch.write(chunk(slot(0), 0, 6, vec![4; 6]))).is_err());
    assert_eq!(
        std::fs::metadata(root.join("segment-00-00000001.bin"))
            .expect("len")
            .len(),
        4096
    );
}

#[test]
fn slot_geometry_rejects_missing_prefix_oversize_and_old_slot_overwrite() {
    let temp = tempfile::tempdir().expect("root");
    let root = temp.path().join("device");
    let device = device(root.clone());
    let mut batch = device.begin_write_batch().expect("begin");
    assert!(block_on(batch.write(chunk(slot(1), 0, 1, vec![1]))).is_err());
    assert!(block_on(batch.write(chunk(slot(0), 0, 4097, vec![1]))).is_err());
    block_on(batch.write(chunk(slot(0), 0, 1, vec![3]))).expect("first");
    block_on(batch.write(chunk(slot(1), 0, 2, vec![7; 2]))).expect("strict slot tail");
    drop(batch);
    let before = std::fs::read(root.join("segment-00-00000001.bin")).expect("segment");
    assert_eq!(before.len(), 8192);
    assert_eq!(before[0], 3);
    assert_eq!(&before[4096..4098], &[7, 7]);
    let mut batch = device.begin_write_batch().expect("next");
    assert!(block_on(batch.write(chunk(slot(0), 0, 1, vec![9]))).is_err());
    drop(batch);
    assert_eq!(
        std::fs::read(root.join("segment-00-00000001.bin")).expect("unchanged"),
        before
    );
}

#[test]
fn large_over_one_mib_and_independent_objects_need_no_global_sort() {
    let temp = tempfile::tempdir().expect("root");
    let root = temp.path().join("device");
    let device = device(root.clone());
    let large = BlockId::large(pi_block_device::LargeBlockId::new(1).expect("id"));
    let len = (1 << 20) + 17;
    let mut batch = device.begin_write_batch().expect("begin");
    block_on(batch.write(chunk(large, 0, len, vec![9; 1 << 20]))).expect("large prefix");
    block_on(batch.write(chunk(slot(0), 0, 1, vec![3]))).expect("interleaved ordinary");
    block_on(batch.write(chunk(large, 1 << 20, len, vec![7; 17]))).expect("large suffix");
    block_on(batch.prepare()).expect("large and ordinary synced");
    let bytes = std::fs::read(root.join("large-000000000000001.bin")).expect("large");
    assert_eq!(bytes.len() as u64, len);
    assert!(bytes[..1 << 20].iter().all(|b| *b == 9));
    assert_eq!(&bytes[1 << 20..], &[7; 17]);
    let mut batch = device.begin_write_batch().expect("next");
    assert!(block_on(batch.write(chunk(large, 0, len, vec![1]))).is_err());
}

#[test]
fn checked_chunks_reject_overflow_and_invalid_ranges() {
    assert!(BlockWritePosition::new(0, 0).is_err());
    assert!(BlockWritePosition::new(7, 6).is_err());
    for (offset, len, bytes) in [
        (0, 6, vec![]),
        (5, 6, vec![1; 2]),
        (u64::MAX, u64::MAX, vec![1]),
    ] {
        assert!(
            BlockWriteChunk::new(
                slot(0),
                Bytes::from(bytes),
                BlockWritePosition::new(offset, len).expect("position")
            )
            .is_err()
        );
    }
}

#[test]
fn real_io_failure_preserves_source_and_fences_same_instance() {
    let temp = tempfile::tempdir().expect("root");
    let root = temp.path().join("device");
    let device = device(root.clone());
    // Fixture precedes any mapping and creates an actual open-append failure.
    std::fs::create_dir(root.join("segment-00-00000001.bin")).expect("conflicting directory");
    let mut batch = device.begin_write_batch().expect("begin");
    let error = block_on(batch.write(chunk(slot(0), 0, 1, vec![1]))).expect_err("real I/O failure");
    assert_eq!(error.operation, pi_block_device::DeviceOperation::Write);
    assert!(!format!("{:?}", error.source).contains("Unimplemented"));
    assert!(block_on(batch.write(chunk(slot(0), 0, 1, vec![1]))).is_err());
    drop(batch);
    assert!(device.begin_write_batch().is_err());
    assert!(root.join("segment-00-00000001.bin").is_dir());
}

#[test]
fn drop_partial_batch_preserves_bytes_and_fences_reuse() {
    let temp = tempfile::tempdir().expect("root");
    let root = temp.path().join("device");
    let device = device(root.clone());
    let mut batch = device.begin_write_batch().expect("begin");
    block_on(batch.write(chunk(slot(0), 0, 6, vec![3; 2]))).expect("prefix");
    drop(batch);
    assert!(device.begin_write_batch().is_err());
    assert_eq!(
        std::fs::read(root.join("segment-00-00000001.bin")).expect("residue"),
        vec![3; 2]
    );
}
