use rpds::RedBlackTreeMapSync;
use std::path::PathBuf;

use bytes::Bytes;
use futures_lite::future::block_on;
use pi_append_log::DefaultBlockCodec;
use pi_block_device::{
    BlockDeviceId, BlockDeviceState, DeviceKey, LargeBlockData, LargeBlockId, RecordSeq,
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

fn seq(value: u32) -> RecordSeq {
    RecordSeq::new(value).expect("valid sequence")
}
fn device() -> DeviceKey {
    DeviceKey::new(1).expect("valid device")
}
fn id(value: u64) -> LargeBlockId {
    LargeBlockId::new(value).expect("valid large block id")
}
fn devices() -> DeviceTable {
    DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
            label: DeviceLabel::new("primary".to_owned()).expect("valid label"),
        },
        BlockDeviceState::initial().expect("initial state"),
    )])
    .expect("one-device table")
}
fn allocate_on(device: DeviceKey, block_id: u64, logical_len: u64) -> TransactionChange<()> {
    TransactionChange::System(SystemChange::AllocateLargeBlock {
        device,
        id: id(block_id),
        logical_len,
    })
}
fn allocate(block_id: u64, logical_len: u64) -> TransactionChange<()> {
    allocate_on(device(), block_id, logical_len)
}
fn release_on(device: DeviceKey, block_id: u64) -> TransactionChange<()> {
    TransactionChange::System(SystemChange::ReleaseLargeBlock {
        device,
        id: id(block_id),
    })
}
fn release(block_id: u64) -> TransactionChange<()> {
    release_on(device(), block_id)
}
fn remove_on(device: DeviceKey, block_id: u64) -> TransactionChange<()> {
    TransactionChange::System(SystemChange::RemoveLargeBlock {
        device,
        id: id(block_id),
    })
}
fn remove(block_id: u64) -> TransactionChange<()> {
    remove_on(device(), block_id)
}
fn unknown_device() -> DeviceKey {
    DeviceKey::new(2).expect("valid unknown device")
}
fn assert_invalid_recovery(
    result: Result<
        (
            SuperblockAppendLog,
            pi_superblock::RecoveredState<TestState>,
        ),
        OpenError,
    >,
) {
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("recovery must fail"),
    };
    let OpenError::RecoveryFailed { kind, context } = error else {
        unreachable!("recovery returned an unexpected error")
    };
    assert_eq!(kind, RecoveryFailureKind::Decode);
    assert_eq!(context.operation, RecoveryOperation::DecodeDelta);
    assert!(context.source.downcast_ref::<SuperblockError>().is_some());
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
fn assert_common(state: &pi_superblock::SuperblockState<TestState>, expected_seq: u32) {
    assert_eq!(state.record_seq, seq(expected_seq));
    assert_eq!(state.application, TestState(7));
    assert!(state.roots.is_empty());
    assert!(state.bindings.is_empty());
    let device_state = state.devices.get(device()).expect("device exists").state();
    let initial = BlockDeviceState::initial().expect("initial state");
    assert_eq!(device_state.levels, initial.levels);
}

#[test]
fn recovery_replays_large_block_allocation_with_large_logical_length() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("allocate-large");
        let max = write_log(path.clone(), vec![vec![allocate(1, 524_289)]]).await;
        let (_, recovered) = pi_superblock::build_recovering(path, max, &TestCodec)
            .await
            .expect("allocation replays");
        assert_common(&recovered.state, 2);
        assert_eq!(recovered.status, SuperblockStatus::Ready);
        let large = &recovered
            .state
            .devices
            .get(device())
            .unwrap()
            .state()
            .large_blocks;
        assert_eq!(large.next_id.raw(), 2);
        assert_eq!(
            large.entries.get(&id(1)),
            Some(&LargeBlockData {
                allocated_at: seq(2),
                released_at: RecordSeq::NULL,
                logical_len: 524_289
            })
        );
    });
}

#[test]
fn recovery_replays_two_large_allocations_in_same_delta_order() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("allocate-two-large");
        let max = write_log(
            path.clone(),
            vec![vec![allocate(1, 524_289), allocate(2, 7)]],
        )
        .await;
        let (_, recovered) = pi_superblock::build_recovering(path, max, &TestCodec)
            .await
            .expect("allocations replay");
        assert_common(&recovered.state, 2);
        let large = &recovered
            .state
            .devices
            .get(device())
            .unwrap()
            .state()
            .large_blocks;
        assert_eq!(large.next_id.raw(), 3);
        assert_eq!(
            large.entries.keys().copied().collect::<Vec<_>>(),
            vec![id(1), id(2)]
        );
        assert_eq!(
            large.entries.get(&id(1)),
            Some(&LargeBlockData {
                allocated_at: seq(2),
                released_at: RecordSeq::NULL,
                logical_len: 524_289
            })
        );
        assert_eq!(
            large.entries.get(&id(2)),
            Some(&LargeBlockData {
                allocated_at: seq(2),
                released_at: RecordSeq::NULL,
                logical_len: 7
            })
        );
    });
}

#[test]
fn recovery_replays_large_block_release_at_following_sequence() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("release-large");
        let max = write_log(
            path.clone(),
            vec![vec![allocate(1, 524_289)], vec![release(1)]],
        )
        .await;
        let (_, recovered) = pi_superblock::build_recovering(path, max, &TestCodec)
            .await
            .expect("release replays");
        assert_common(&recovered.state, 3);
        assert_eq!(recovered.status, SuperblockStatus::Ready);
        let large = &recovered
            .state
            .devices
            .get(device())
            .unwrap()
            .state()
            .large_blocks;
        assert_eq!(large.next_id.raw(), 2);
        assert_eq!(large.entries.get(&id(1)).unwrap().released_at, seq(3));
    });
}

#[test]
fn recovery_replays_large_block_remove_without_advancing_next_id() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("remove-large");
        let max = write_log(
            path.clone(),
            vec![
                vec![allocate(1, 524_289)],
                vec![release(1)],
                vec![remove(1)],
            ],
        )
        .await;
        let (_, recovered) = pi_superblock::build_recovering(path, max, &TestCodec)
            .await
            .expect("remove replays");
        assert_common(&recovered.state, 4);
        assert_eq!(recovered.status, SuperblockStatus::Ready);
        let large = &recovered
            .state
            .devices
            .get(device())
            .unwrap()
            .state()
            .large_blocks;
        assert_eq!(large.next_id.raw(), 2);
        assert!(large.entries.is_empty());
        assert!(large.validate());
    });
}

#[test]
fn recovery_rejects_large_block_allocation_for_unknown_device() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("allocate-unknown-device");
        let max = write_log(
            path.clone(),
            vec![vec![allocate_on(unknown_device(), 1, 7)]],
        )
        .await;
        let result = pi_superblock::build_recovering(path, max, &TestCodec).await;
        assert_invalid_recovery(result);
    });
}

#[test]
fn recovery_rejects_large_block_allocation_when_id_skips_next_id() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("allocate-skipped-id");
        let max = write_log(path.clone(), vec![vec![allocate(2, 7)]]).await;
        let result = pi_superblock::build_recovering(path, max, &TestCodec).await;
        assert_invalid_recovery(result);
    });
}

#[test]
fn recovery_rejects_duplicate_large_block_allocation_in_one_delta() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("allocate-duplicate-id");
        let max = write_log(path.clone(), vec![vec![allocate(1, 7), allocate(1, 9)]]).await;
        let result = pi_superblock::build_recovering(path, max, &TestCodec).await;
        assert_invalid_recovery(result);
    });
}

#[test]
fn recovery_rejects_release_of_unknown_large_block() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("release-unknown-id");
        let max = write_log(path.clone(), vec![vec![release(2)]]).await;
        let result = pi_superblock::build_recovering(path, max, &TestCodec).await;
        assert_invalid_recovery(result);
    });
}

#[test]
fn recovery_rejects_repeated_large_block_release_across_deltas() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("release-repeated-id");
        let max = write_log(
            path.clone(),
            vec![vec![allocate(1, 7)], vec![release(1)], vec![release(1)]],
        )
        .await;
        let result = pi_superblock::build_recovering(path, max, &TestCodec).await;
        assert_invalid_recovery(result);
    });
}

#[test]
fn recovery_rejects_removing_unreleased_large_block() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("remove-unreleased-id");
        let max = write_log(path.clone(), vec![vec![allocate(1, 7)], vec![remove(1)]]).await;
        let result = pi_superblock::build_recovering(path, max, &TestCodec).await;
        assert_invalid_recovery(result);
    });
}

#[test]
fn recovery_rejects_removing_unknown_large_block() {
    block_on(async {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("remove-unknown-id");
        let max = write_log(path.clone(), vec![vec![remove(2)]]).await;
        let result = pi_superblock::build_recovering(path, max, &TestCodec).await;
        assert_invalid_recovery(result);
    });
}

#[test]
fn recovery_merges_same_sequence_runs_and_trims_more_than_two_versions() {
    use pi_block_device::{BlockRange, SegmentVersion};
    block_on(async {
        let temp = tempdir().unwrap();
        let path = temp.path().join("segment-window");
        let allocate = |version| {
            TransactionChange::System(SystemChange::AllocateSegmentBlocks {
                device: device(),
                level: 0,
                version: SegmentVersion::new(version).unwrap(),
                len: 1,
            })
        };
        let create = |version| {
            TransactionChange::System(SystemChange::CreateActiveSegment {
                device: device(),
                level: 0,
                version: SegmentVersion::new(version).unwrap(),
            })
        };
        let release = |version| {
            TransactionChange::System(SystemChange::ReleaseSegmentBlocks {
                device: device(),
                level: 0,
                version: SegmentVersion::new(version).unwrap(),
                ranges: vec![BlockRange {
                    start: 0,
                    len: std::num::NonZeroU32::new(if version == 1 { 2 } else { 1 }).unwrap(),
                }],
            })
        };
        let remove = |version| {
            TransactionChange::System(SystemChange::RemoveSegment {
                device: device(),
                level: 0,
                version: SegmentVersion::new(version).unwrap(),
            })
        };
        let max = write_log(
            path.clone(),
            vec![
                vec![allocate(1), allocate(1)],
                vec![create(2), allocate(2), create(3), allocate(3)],
                vec![release(1), release(2), remove(2), remove(1)],
            ],
        )
        .await;
        let (_, recovered) = pi_superblock::build_recovering(path, max, &TestCodec)
            .await
            .unwrap();
        let state = recovered.state.devices.get(device()).unwrap().state();
        assert_eq!(state.levels[0].min_version.raw(), 3);
        assert_eq!(state.levels[0].versions.len(), 1);
        let segment = state.levels[0].versions.last().unwrap().as_ref().unwrap();
        assert_eq!(segment.allocation_runs.len(), 1);
        assert_eq!(segment.allocation_runs.get(0).unwrap().allocated_at, seq(3));
        assert_eq!(
            segment.released_at.iter().copied().collect::<Vec<_>>(),
            vec![RecordSeq::NULL]
        );
        assert!(state.validate_at(seq(4)));
    });
}

#[test]
fn recovery_rejects_partial_release_change_without_returning_candidate() {
    use pi_block_device::{BlockRange, SegmentVersion};
    block_on(async {
        let temp = tempdir().unwrap();
        let path = temp.path().join("partial-release");
        let allocation = TransactionChange::System(SystemChange::AllocateSegmentBlocks {
            device: device(),
            level: 0,
            version: SegmentVersion::new(1).unwrap(),
            len: 2,
        });
        let release = TransactionChange::System(SystemChange::ReleaseSegmentBlocks {
            device: device(),
            level: 0,
            version: SegmentVersion::new(1).unwrap(),
            ranges: vec![BlockRange {
                start: 0,
                len: std::num::NonZeroU32::new(3).unwrap(),
            }],
        });
        let max = write_log(path.clone(), vec![vec![allocation], vec![release]]).await;
        assert_invalid_recovery(pi_superblock::build_recovering(path, max, &TestCodec).await);
    });
}
