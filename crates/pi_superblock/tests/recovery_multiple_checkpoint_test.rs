use rpds::RedBlackTreeMapSync;

use bytes::Bytes;
use futures_lite::future::block_on;
use pi_append_log::DefaultBlockCodec;
use pi_block_device::{
    AllocationRun, BlockDeviceId, BlockDeviceState, BlockId, BlockRef, DeviceKey, RecordSeq,
    SegmentVersion,
};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, BindingName,
    DeviceLabel, DeviceRegistration, DeviceSlot, DeviceTable, DurableAppendOutcome, OpenError,
    RecordKind, RecoveryFailureKind, SuperblockAppendLog,
};
use tempfile::tempdir;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState(u32);

impl ApplicationState for TestState {
    type Delta = u32;
}

struct TestCodec;

impl ApplicationCodec<TestState> for TestCodec {
    fn encode_state(
        &self,
        state: &TestState,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u32_le(state.0)
    }

    fn decode_state(&self, bytes: Bytes) -> Result<TestState, ApplicationCodecError> {
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("invalid test state"))?;
        Ok(TestState(u32::from_le_bytes(raw)))
    }

    fn encode_delta(
        &self,
        delta: &u32,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u32_le(*delta)
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<u32, ApplicationCodecError> {
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("invalid test delta"))?;
        Ok(u32::from_le_bytes(raw))
    }
}

fn legal_checkpoint_parts() -> (
    DeviceTable,
    RedBlackTreeMapSync<BlockRef, RecordSeq>,
    RedBlackTreeMapSync<BindingName, BlockRef>,
) {
    let allocation_seq = RecordSeq::new(1).expect("record sequence 1 must be valid");
    let device_key = DeviceKey::new(1).expect("device key 1 must be valid");
    let mut device_state = BlockDeviceState::initial().expect("initial device state");
    let active_segment = device_state.levels[0]
        .versions
        .get_mut(0)
        .and_then(Option::as_mut)
        .expect("initial active segment");
    active_segment.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: allocation_seq,
        len: 1,
    });
    active_segment.released_at.push_back_mut(RecordSeq::NULL);

    let live_block = BlockRef {
        device: device_key,
        block: BlockId::ordinary(
            0,
            SegmentVersion::new(1).expect("segment version 1 must be valid"),
            0,
        )
        .expect("ordinary live block must be valid"),
    };
    let devices = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: BlockDeviceId::from_bytes([1; 16]).expect("device id must be valid"),
            label: DeviceLabel::new("primary".to_owned()).expect("device label must be valid"),
        },
        device_state,
    )])
    .expect("device table must be valid");
    let roots = [(live_block, allocation_seq)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    let binding_name = BindingName::new(b"main".to_vec()).expect("binding name must be valid");
    let bindings = [(binding_name, live_block)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    (devices, roots, bindings)
}

#[test]
fn build_recovering_uses_the_later_checkpoint_as_current_state() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("multiple-checkpoint-log");
        let codec = DefaultBlockCodec::default();
        let max_payload_len = codec.max_payload_len();
        let seq1 = RecordSeq::new(1).expect("record sequence 1 must be valid");
        let seq2 = RecordSeq::new(2).expect("record sequence 2 must be valid");
        let (devices, roots, bindings) = legal_checkpoint_parts();
        let checkpoint_one = pi_superblock::encode_checkpoint(
            seq1,
            &devices,
            &roots,
            &bindings,
            &TestState(11),
            &TestCodec,
        )
        .expect("first legal checkpoint must encode");
        let checkpoint_two = pi_superblock::encode_checkpoint(
            seq2,
            &devices,
            &roots,
            &bindings,
            &TestState(22),
            &TestCodec,
        )
        .expect("second legal checkpoint must encode");

        let log = SuperblockAppendLog::open(path.clone(), max_payload_len)
            .await
            .expect("empty real file append log must open");
        assert!(
            matches!(
                log.append_durable(seq1, RecordKind::Checkpoint, checkpoint_one)
                    .await,
                DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq1
            ),
            "first checkpoint must be durable"
        );
        assert!(
            matches!(
                log.append_durable(seq2, RecordKind::Checkpoint, checkpoint_two)
                    .await,
                DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq2
            ),
            "second checkpoint must be durable"
        );
        drop(log);

        let (_log, recovered) = pi_superblock::build_recovering(path, max_payload_len, &TestCodec)
            .await
            .expect("build_recovering must recover two checkpoints");

        assert_eq!(recovered.state.record_seq, seq2);
        assert_eq!(recovered.state.application, TestState(22));
    });
}

#[test]
fn build_recovering_rejects_a_later_checkpoint_with_corrupt_application_payload() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("corrupt-later-checkpoint-log");
        let codec = DefaultBlockCodec::default();
        let max_payload_len = codec.max_payload_len();
        let seq1 = RecordSeq::new(1).expect("record sequence 1 must be valid");
        let seq2 = RecordSeq::new(2).expect("record sequence 2 must be valid");
        let (devices, roots, bindings) = legal_checkpoint_parts();
        let checkpoint_one = pi_superblock::encode_checkpoint(
            seq1,
            &devices,
            &roots,
            &bindings,
            &TestState(11),
            &TestCodec,
        )
        .expect("first legal checkpoint must encode");
        let checkpoint_two = pi_superblock::encode_checkpoint(
            seq2,
            &devices,
            &roots,
            &bindings,
            &TestState(22),
            &TestCodec,
        )
        .expect("second legal checkpoint must encode");
        let corrupt_checkpoint =
            Bytes::copy_from_slice(&checkpoint_two[..checkpoint_two.len() - 1]);

        let log = SuperblockAppendLog::open(path.clone(), max_payload_len)
            .await
            .expect("empty real file append log must open");
        assert!(matches!(
            log.append_durable(seq1, RecordKind::Checkpoint, checkpoint_one)
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq1
        ));
        assert!(matches!(
            log.append_durable(seq2, RecordKind::Checkpoint, corrupt_checkpoint)
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq2
        ));
        drop(log);

        let result = pi_superblock::build_recovering(path, max_payload_len, &TestCodec).await;
        match result {
            Err(OpenError::RecoveryFailed { kind, .. }) => {
                assert_eq!(kind, RecoveryFailureKind::Decode);
            }
            Ok(_) => panic!("corrupt later checkpoint must not return partial recovered state"),
            Err(other) => panic!("expected checkpoint decode failure, got {other:?}"),
        }
    });
}

#[test]
fn build_recovering_rejects_a_later_delta_as_decode_failure() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("later-delta-recovery-log");
        let codec = DefaultBlockCodec::default();
        let max_payload_len = codec.max_payload_len();
        let seq1 = RecordSeq::new(1).expect("record sequence 1 must be valid");
        let seq2 = RecordSeq::new(2).expect("record sequence 2 must be valid");
        let (devices, roots, bindings) = legal_checkpoint_parts();
        let checkpoint = pi_superblock::encode_checkpoint(
            seq1,
            &devices,
            &roots,
            &bindings,
            &TestState(11),
            &TestCodec,
        )
        .expect("legal checkpoint must encode");

        let log = SuperblockAppendLog::open(path.clone(), max_payload_len)
            .await
            .expect("empty real file append log must open");
        assert!(matches!(
            log.append_durable(seq1, RecordKind::Checkpoint, checkpoint)
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq1
        ));
        assert!(matches!(
            log.append_durable(seq2, RecordKind::Delta, Bytes::from_static(b"delta"))
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq2
        ));
        drop(log);

        let result = pi_superblock::build_recovering(path, max_payload_len, &TestCodec).await;
        match result {
            Err(OpenError::RecoveryFailed { kind, .. }) => {
                assert_eq!(kind, RecoveryFailureKind::Decode);
            }
            Ok(_) => panic!("later Delta must not return recovered state"),
            Err(other) => panic!("expected Delta recovery failure, got {other:?}"),
        }
    });
}

#[test]
fn build_recovering_rejects_a_repeated_later_record_sequence_as_decode_failure() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("repeated-later-sequence-log");
        let codec = DefaultBlockCodec::default();
        let max_payload_len = codec.max_payload_len();
        let seq1 = RecordSeq::new(1).expect("record sequence 1 must be valid");
        let (devices, roots, bindings) = legal_checkpoint_parts();
        let checkpoint = pi_superblock::encode_checkpoint(
            seq1,
            &devices,
            &roots,
            &bindings,
            &TestState(11),
            &TestCodec,
        )
        .expect("legal checkpoint must encode");

        let log = SuperblockAppendLog::open(path.clone(), max_payload_len)
            .await
            .expect("empty real file append log must open");
        for body in [checkpoint.clone(), checkpoint] {
            assert!(matches!(
                log.append_durable(seq1, RecordKind::Checkpoint, body).await,
                DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq1
            ));
        }
        drop(log);

        let result = pi_superblock::build_recovering(path, max_payload_len, &TestCodec).await;
        match result {
            Err(OpenError::RecoveryFailed { kind, .. }) => {
                assert_eq!(kind, RecoveryFailureKind::Decode);
            }
            Ok(_) => panic!("repeated later sequence must not return recovered state"),
            Err(other) => panic!("expected repeated-sequence failure, got {other:?}"),
        }
    });
}

#[test]
fn build_recovering_rejects_a_decreasing_later_record_sequence_as_decode_failure() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("decreasing-later-sequence-log");
        let codec = DefaultBlockCodec::default();
        let max_payload_len = codec.max_payload_len();
        let seq1 = RecordSeq::new(1).expect("record sequence 1 must be valid");
        let seq2 = RecordSeq::new(2).expect("record sequence 2 must be valid");
        let (devices, roots, bindings) = legal_checkpoint_parts();
        let checkpoint_one = pi_superblock::encode_checkpoint(
            seq2,
            &devices,
            &roots,
            &bindings,
            &TestState(22),
            &TestCodec,
        )
        .expect("first legal checkpoint must encode");
        let checkpoint_two = pi_superblock::encode_checkpoint(
            seq1,
            &devices,
            &roots,
            &bindings,
            &TestState(11),
            &TestCodec,
        )
        .expect("second legal checkpoint must encode");

        let log = SuperblockAppendLog::open(path.clone(), max_payload_len)
            .await
            .expect("empty real file append log must open");
        assert!(matches!(
            log.append_durable(seq2, RecordKind::Checkpoint, checkpoint_one)
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq2
        ));
        assert!(matches!(
            log.append_durable(seq1, RecordKind::Checkpoint, checkpoint_two)
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq1
        ));
        drop(log);

        let result = pi_superblock::build_recovering(path, max_payload_len, &TestCodec).await;
        match result {
            Err(OpenError::RecoveryFailed { kind, .. }) => {
                assert_eq!(kind, RecoveryFailureKind::Decode);
            }
            Ok(_) => panic!("decreasing later sequence must not return recovered state"),
            Err(other) => panic!("expected decreasing-sequence failure, got {other:?}"),
        }
    });
}
