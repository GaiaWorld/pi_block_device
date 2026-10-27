use rpds::RedBlackTreeMapSync;
use std::num::NonZeroU32;
use std::path::PathBuf;

use bytes::Bytes;
use futures_lite::future::block_on;
use pi_append_log::DefaultBlockCodec;
use pi_block_device::{
    AllocationRun, BlockDeviceId, BlockDeviceState, BlockRange, DeviceKey, LevelVersions,
    RecordSeq, SegmentData, SegmentVersion,
};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, BindingName,
    DeviceLabel, DeviceRegistration, DeviceSlot, DeviceTable, DurableAppendOutcome, OpenError,
    RecordKind, RecoveryFailureKind, RecoveryOperation, SuperblockAppendLog, SuperblockError,
    SuperblockStatus, SystemChange, TransactionChange, encode_checkpoint, encode_delta,
};
use tempfile::tempdir;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState(u32);
impl ApplicationState for TestState {
    type Delta = ();
}

struct TestCodec;
impl ApplicationCodec<TestState> for TestCodec {
    fn encode_state(
        &self,
        state: &TestState,
        out: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        out.put_u32_le(state.0)
    }
    fn decode_state(&self, bytes: Bytes) -> Result<TestState, ApplicationCodecError> {
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("test state must be u32"))?;
        Ok(TestState(u32::from_le_bytes(raw)))
    }
    fn encode_delta(
        &self,
        _: &(),
        _: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
    }
    fn decode_delta(&self, bytes: Bytes) -> Result<(), ApplicationCodecError> {
        if bytes.is_empty() {
            Ok(())
        } else {
            Err(ApplicationCodecError::new("test delta must be empty"))
        }
    }
}

fn seq(n: u32) -> RecordSeq {
    RecordSeq::new(n).expect("valid sequence")
}
fn ver(n: u32) -> SegmentVersion {
    SegmentVersion::new(n).expect("valid version")
}
fn device() -> DeviceKey {
    DeviceKey::new(1).expect("valid device")
}
fn range(start: u32, len: u32) -> BlockRange {
    BlockRange {
        start,
        len: NonZeroU32::new(len).expect("nonzero range"),
    }
}
fn devices() -> DeviceTable {
    DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
            label: DeviceLabel::new("primary".into()).expect("valid label"),
        },
        BlockDeviceState::initial().expect("initial state"),
    )])
    .expect("one-device table")
}
fn system(change: SystemChange) -> TransactionChange<()> {
    TransactionChange::System(change)
}
fn allocate(version: u32, len: u32) -> TransactionChange<()> {
    system(SystemChange::AllocateSegmentBlocks {
        device: device(),
        level: 0,
        version: ver(version),
        len,
    })
}
fn create(version: u32) -> TransactionChange<()> {
    system(SystemChange::CreateActiveSegment {
        device: device(),
        level: 0,
        version: ver(version),
    })
}
fn release(version: u32, ranges: Vec<BlockRange>) -> TransactionChange<()> {
    system(SystemChange::ReleaseSegmentBlocks {
        device: device(),
        level: 0,
        version: ver(version),
        ranges,
    })
}
fn remove(version: u32) -> TransactionChange<()> {
    system(SystemChange::RemoveSegment {
        device: device(),
        level: 0,
        version: ver(version),
    })
}
async fn write_log(path: PathBuf, deltas: Vec<Vec<TransactionChange<()>>>) -> usize {
    let max = DefaultBlockCodec::default().max_payload_len();
    let checkpoint = encode_checkpoint(
        seq(1),
        &devices(),
        &RedBlackTreeMapSync::new_sync(),
        &RedBlackTreeMapSync::<BindingName, pi_block_device::BlockRef>::new_sync(),
        &TestState(7),
        &TestCodec,
    )
    .expect("checkpoint encodes");
    let log = SuperblockAppendLog::open(path, max)
        .await
        .expect("real log opens");
    assert!(matches!(
        log.append_durable(seq(1), RecordKind::Checkpoint, checkpoint)
            .await,
        DurableAppendOutcome::Durable { .. }
    ));
    for (index, changes) in deltas.iter().enumerate() {
        let record_seq = seq(u32::try_from(index).expect("small fixture") + 2);
        let body = encode_delta::<TestState, TestCodec>(record_seq, changes, &TestCodec)
            .expect("delta encodes");
        assert!(matches!(
            log.append_durable(record_seq, RecordKind::Delta, body)
                .await,
            DurableAppendOutcome::Durable { .. }
        ));
    }
    drop(log);
    max
}

#[test]
fn recovery_allocates_two_slots_in_v1_at_seq2() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("allocate-v1");
        let max = write_log(path.clone(), vec![vec![allocate(1, 2)]]).await;
        let (_, recovered) = pi_superblock::build_recovering(path, max, &TestCodec)
            .await
            .expect("allocation replays");
        let level = &recovered
            .state
            .devices
            .get(device())
            .unwrap()
            .state()
            .levels[0];
        assert_eq!(recovered.state.record_seq, seq(2));
        assert_eq!(recovered.state.application, TestState(7));
        assert_eq!(recovered.status, SuperblockStatus::Ready);
        assert_eq!(
            level,
            &LevelVersions {
                min_version: ver(1),
                versions: rpds::vector_sync![Some(SegmentData {
                    allocation_runs: rpds::vector_sync![AllocationRun {
                        allocated_at: seq(2),
                        len: 2
                    }],
                    released_at: rpds::vector_sync![RecordSeq::NULL, RecordSeq::NULL],
                })],
            }
        );
    });
}

#[test]
fn recovery_seals_v1_then_creates_and_allocates_tail_v2_in_order() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("create-v2");
        let max = write_log(
            path.clone(),
            vec![vec![allocate(1, 1)], vec![create(2), allocate(2, 1)]],
        )
        .await;
        let (_, recovered) = pi_superblock::build_recovering(path, max, &TestCodec)
            .await
            .expect("ordered create and allocate replay");
        let level = &recovered
            .state
            .devices
            .get(device())
            .unwrap()
            .state()
            .levels[0];
        assert_eq!(recovered.state.record_seq, seq(3));
        assert_eq!(recovered.state.application, TestState(7));
        assert_eq!(recovered.status, SuperblockStatus::Ready);
        assert_eq!(
            level,
            &LevelVersions {
                min_version: ver(1),
                versions: rpds::vector_sync![
                    Some(SegmentData {
                        allocation_runs: rpds::vector_sync![AllocationRun {
                            allocated_at: seq(2),
                            len: 1
                        }],
                        released_at: rpds::vector_sync![RecordSeq::NULL],
                    }),
                    Some(SegmentData {
                        allocation_runs: rpds::vector_sync![AllocationRun {
                            allocated_at: seq(3),
                            len: 1
                        }],
                        released_at: rpds::vector_sync![RecordSeq::NULL],
                    })
                ],
            }
        );
        assert!(
            level.versions.get(0).unwrap().is_some(),
            "v1 remains sealed"
        );
        assert!(
            level.versions.last().unwrap().is_some(),
            "v2 is the active tail"
        );
    });
}

#[test]
fn recovery_releases_adjacent_ranges_at_current_sequence() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("adjacent-release");
        let max = write_log(
            path.clone(),
            vec![
                vec![allocate(1, 3)],
                vec![release(1, vec![range(0, 1), range(1, 1)])],
            ],
        )
        .await;
        let (_, recovered) = pi_superblock::build_recovering(path, max, &TestCodec)
            .await
            .expect("adjacent releases replay");
        let level = &recovered
            .state
            .devices
            .get(device())
            .unwrap()
            .state()
            .levels[0];
        assert_eq!(recovered.state.record_seq, seq(3));
        assert_eq!(recovered.state.application, TestState(7));
        assert_eq!(recovered.status, SuperblockStatus::Ready);
        assert_eq!(
            level,
            &LevelVersions {
                min_version: ver(1),
                versions: rpds::vector_sync![Some(SegmentData {
                    allocation_runs: rpds::vector_sync![AllocationRun {
                        allocated_at: seq(2),
                        len: 3
                    }],
                    released_at: rpds::vector_sync![seq(3), seq(3), RecordSeq::NULL],
                })],
            }
        );
    });
}

#[test]
fn recovery_removes_fully_released_sealed_v1_and_trims_window() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("remove-v1");
        let max = write_log(
            path.clone(),
            vec![
                vec![allocate(1, 1)],
                vec![create(2), allocate(2, 1)],
                vec![release(1, vec![range(0, 1)])],
                vec![remove(1)],
            ],
        )
        .await;
        let (_, recovered) = pi_superblock::build_recovering(path, max, &TestCodec)
            .await
            .expect("released sealed segment removal replays");
        let level = &recovered
            .state
            .devices
            .get(device())
            .unwrap()
            .state()
            .levels[0];
        assert_eq!(recovered.state.record_seq, seq(5));
        assert_eq!(recovered.state.application, TestState(7));
        assert_eq!(recovered.status, SuperblockStatus::Ready);
        assert_eq!(
            level,
            &LevelVersions {
                min_version: ver(2),
                versions: rpds::vector_sync![Some(SegmentData {
                    allocation_runs: rpds::vector_sync![AllocationRun {
                        allocated_at: seq(3),
                        len: 1
                    }],
                    released_at: rpds::vector_sync![RecordSeq::NULL],
                })],
            }
        );
        assert!(
            ver(1).raw() < level.min_version.raw(),
            "removed v1 is outside the window"
        );
    });
}

fn assert_invalid_segment_recovery(error: OpenError) {
    match error {
        OpenError::RecoveryFailed { kind, context } => {
            assert_eq!(kind, RecoveryFailureKind::Decode);
            assert_eq!(context.operation, RecoveryOperation::DecodeDelta);
            assert!(matches!(
                context.source.downcast_ref::<SuperblockError>(),
                Some(SuperblockError::InvalidBlockRef)
            ));
        }
        OpenError::NotInitialized => panic!("recovery unexpectedly reported an uninitialized log"),
    }
}

#[test]
fn recovery_rejects_create_active_segment_without_existing_sealed_segment() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("create-v2-without-v1");
        let max = write_log(path.clone(), vec![vec![create(2)]]).await;
        let result = pi_superblock::build_recovering(path, max, &TestCodec).await;
        match result {
            Ok(_) => panic!("invalid active segment creation unexpectedly recovered"),
            Err(error) => assert_invalid_segment_recovery(error),
        }
    });
}

#[test]
fn recovery_rejects_create_then_remove_of_missing_old_active_segment() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("create-then-remove-v1");
        let max = write_log(path.clone(), vec![vec![create(2), remove(1)]]).await;
        let result = pi_superblock::build_recovering(path, max, &TestCodec).await;
        match result {
            Ok(_) => panic!("create then remove unexpectedly recovered"),
            Err(error) => assert_invalid_segment_recovery(error),
        }
    });
}
