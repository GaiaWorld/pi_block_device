use std::error::Error;
use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use pi_block_device::{
    AllocationRun, BlockDeviceState, BlockDeviceWriteBatch, BlockWriteChunk, BlockWritePosition,
    DeviceError, DeviceOperation, LargeBlockCursor, LargeBlockData, LargeBlockId, LargeBlocks,
    LevelVersions, ProtectedReadBuf, ReconcileError, ReconcileFailureKind, ReconcileProgress,
    RecordSeq, SegmentData, SegmentVersion,
};
use rpds::{RedBlackTreeMapSync, VectorSync};

// @test @api: allocation_state and BlockDeviceWriteBatch
// @design-doc: design/features/{03-backend-session,13-wire-format}.feature

#[derive(Debug)]
struct TestError;
impl fmt::Display for TestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("boom")
    }
}
impl Error for TestError {}

struct TestReadBuf {
    bytes: Bytes,
    lease: Arc<()>,
}
impl AsRef<[u8]> for TestReadBuf {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl ProtectedReadBuf for TestReadBuf {
    type Lease = Arc<()>;
    fn lease(&self) -> &Self::Lease {
        &self.lease
    }
}

struct WriteBatch;
impl BlockDeviceWriteBatch for WriteBatch {
    fn write(
        &mut self,
        _: BlockWriteChunk,
    ) -> impl Future<Output = Result<(), DeviceError>> + Send + '_ {
        std::future::ready(Ok(()))
    }
    fn prepare(self) -> impl Future<Output = Result<(), DeviceError>> + Send {
        std::future::ready(Ok(()))
    }
}

#[test]
fn complete_state_is_constructible_and_validates_lifecycle() {
    let allocated_at = RecordSeq::new(3).expect("allocated sequence");
    let released_at = RecordSeq::new(4).expect("released sequence");
    let segment = SegmentData {
        allocation_runs: rpds::vector_sync![AllocationRun {
            allocated_at,
            len: 2,
        }],
        released_at: rpds::vector_sync![RecordSeq::NULL, released_at],
    };
    assert!(segment.validate());
    assert!(segment.validate_at(RecordSeq::new(4).expect("current sequence")));
    let level = LevelVersions {
        min_version: SegmentVersion::new(1).expect("version"),
        versions: VectorSync::from_iter([Some(segment)]),
    };
    let large_id = LargeBlockId::new(1).expect("large id");
    let state = BlockDeviceState {
        levels: std::array::from_fn(|_| level.clone()),
        large_blocks: LargeBlocks {
            next_id: LargeBlockCursor::new(2).expect("cursor"),
            entries: RedBlackTreeMapSync::from_iter([(
                large_id,
                LargeBlockData {
                    allocated_at,
                    released_at,
                    logical_len: 8192,
                },
            )]),
        },
    };
    assert_eq!(state.levels.len(), 15);
    assert!(state.validate());
    assert!(state.validate_at(RecordSeq::new(4).expect("current sequence")));
    assert!(
        BlockDeviceState::initial()
            .expect("initial state")
            .validate()
    );
}

#[test]
fn allocation_validation_rejects_invalid_order_and_timestamps() {
    let first = RecordSeq::new(3).expect("first sequence");
    let second = RecordSeq::new(4).expect("second sequence");
    let segment = |runs: VectorSync<AllocationRun>| SegmentData {
        allocation_runs: runs,
        released_at: rpds::vector_sync![RecordSeq::NULL, RecordSeq::NULL],
    };

    assert!(
        !segment(rpds::vector_sync![
            AllocationRun {
                allocated_at: second,
                len: 1,
            },
            AllocationRun {
                allocated_at: first,
                len: 1,
            }
        ])
        .validate()
    );
    assert!(
        !segment(rpds::vector_sync![
            AllocationRun {
                allocated_at: first,
                len: 1,
            },
            AllocationRun {
                allocated_at: first,
                len: 1,
            }
        ])
        .validate()
    );
    assert!(
        !segment(rpds::vector_sync![AllocationRun {
            allocated_at: RecordSeq::NULL,
            len: 2,
        }])
        .validate()
    );

    let equal_release = SegmentData {
        allocation_runs: rpds::vector_sync![AllocationRun {
            allocated_at: first,
            len: 1,
        }],
        released_at: rpds::vector_sync![first],
    };
    assert!(!equal_release.validate());

    let future_release = SegmentData {
        allocation_runs: rpds::vector_sync![AllocationRun {
            allocated_at: first,
            len: 1,
        }],
        released_at: rpds::vector_sync![second],
    };
    assert!(!future_release.validate_at(first));
}

#[test]
fn level_validation_allows_non_tail_none_but_rejects_empty_non_tail_segments() {
    let version = SegmentVersion::new(1).expect("version");
    let empty = SegmentData {
        allocation_runs: VectorSync::new_sync(),
        released_at: VectorSync::new_sync(),
    };
    let level = LevelVersions {
        min_version: version,
        versions: VectorSync::from_iter([
            None,
            Some(SegmentData {
                allocation_runs: VectorSync::new_sync(),
                released_at: VectorSync::new_sync(),
            }),
        ]),
    };
    assert!(level.validate());

    let invalid = LevelVersions {
        min_version: version,
        versions: VectorSync::from_iter([
            Some(empty),
            Some(SegmentData {
                allocation_runs: VectorSync::new_sync(),
                released_at: VectorSync::new_sync(),
            }),
        ]),
    };
    assert!(!invalid.validate());

    let invalid_tail = LevelVersions {
        min_version: version,
        versions: VectorSync::from_iter([
            Some(SegmentData {
                allocation_runs: VectorSync::new_sync(),
                released_at: VectorSync::new_sync(),
            }),
            None,
        ]),
    };
    assert!(!invalid_tail.validate());
}

#[test]
fn large_block_validation_rejects_null_allocation_and_cursor_overrun() {
    let released_at = RecordSeq::new(4).expect("released sequence");
    let id = LargeBlockId::new(2).expect("large id");
    let invalid_data = LargeBlockData {
        allocated_at: RecordSeq::NULL,
        released_at,
        logical_len: 1,
    };
    assert!(!invalid_data.validate());
    assert!(
        !LargeBlockData {
            allocated_at: RecordSeq::new(3).expect("allocated sequence"),
            released_at: RecordSeq::new(3).expect("equal release sequence"),
            logical_len: 1,
        }
        .validate()
    );
    assert!(
        !LargeBlockData {
            allocated_at: RecordSeq::new(3).expect("allocated sequence"),
            released_at,
            logical_len: 1,
        }
        .validate_at(RecordSeq::new(3).expect("current sequence"))
    );
    assert!(
        !LargeBlocks {
            next_id: LargeBlockCursor::new(2).expect("cursor"),
            entries: RedBlackTreeMapSync::from_iter([(
                id,
                LargeBlockData {
                    allocated_at: RecordSeq::new(3).expect("allocated sequence"),
                    released_at,
                    logical_len: 1,
                }
            )]),
        }
        .validate()
    );
}

#[test]
fn protected_read_buffer_exposes_lease() {
    fn require<T: ProtectedReadBuf<Lease = Arc<()>>>() {}
    require::<TestReadBuf>();
    let lease = Arc::new(());
    let buffer = TestReadBuf {
        bytes: Bytes::from_static(b"data"),
        lease: Arc::clone(&lease),
    };
    assert!(Arc::ptr_eq(buffer.lease(), &lease));
    assert_eq!(buffer.as_ref(), b"data");
}

#[test]
fn write_batch_accepts_checked_chunk_and_consuming_prepare() {
    let mut batch = WriteBatch;
    let position = BlockWritePosition::new(0, 4).expect("position");
    let block = pi_block_device::BlockId::ordinary(0, SegmentVersion::new(1).unwrap(), 0).unwrap();
    let request = BlockWriteChunk::new(block, Bytes::from_static(b"data"), position).unwrap();
    drop(batch.write(request));
    fn require_send<T: Send>(_: T) {}
    require_send(batch.prepare());
}

#[test]
fn error_text_contains_stable_context() {
    let device = DeviceError {
        operation: DeviceOperation::Read,
        device_key: None,
        source: Box::new(TestError),
    };
    assert_eq!(device.to_string(), "device operation read failed: boom");
    let reconcile = ReconcileError {
        kind: ReconcileFailureKind::RepairIo,
        progress: ReconcileProgress {
            planned_actions: 5,
            completed_actions: 2,
            remaining_actions: 3,
            partial_progress: true,
            media_metadata_synced: false,
        },
        source: Some(Box::new(TestError)),
    };
    assert_eq!(
        reconcile.to_string(),
        "reconcile failed kind=RepairIo completed=2 planned=5: boom"
    );
}
