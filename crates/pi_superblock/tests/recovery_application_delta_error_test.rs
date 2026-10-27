use rpds::RedBlackTreeMapSync;
use std::error::Error;
use std::io;

use bytes::Bytes;
use futures_lite::future::block_on;
use pi_append_log::DefaultBlockCodec;
use pi_block_device::{BlockDeviceId, BlockDeviceState, RecordSeq};
use pi_superblock::{
    ApplicationApplyContext, ApplicationApplyError, ApplicationCodec, ApplicationCodecError,
    ApplicationCodecFailureKind, ApplicationCodecOperation, ApplicationEncoder, ApplicationState,
    BindingName, DeviceLabel, DeviceRegistration, DeviceSlot, DeviceTable, DurableAppendOutcome,
    OpenError, RecordKind, RecoveryFailureKind, RecoveryOperation, SuperblockAppendLog,
    TransactionChange, encode_checkpoint, encode_delta,
};
use tempfile::tempdir;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState(u32);

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestDelta {
    add: u32,
}

impl ApplicationState for TestState {
    type Delta = TestDelta;

    fn apply(
        &self,
        delta: &Self::Delta,
        context: &ApplicationApplyContext<'_>,
    ) -> Result<Self, ApplicationApplyError> {
        if context.change_index == 1 {
            return Err(ApplicationApplyError {
                change_index: context.change_index,
                source: Box::new(io::Error::other("second application change failed")),
            });
        }
        Ok(Self(self.0 + delta.add))
    }
}

struct TestCodec {
    fail_delta_decode: bool,
}

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
            .map_err(|_| ApplicationCodecError::new("test state must be four bytes"))?;
        Ok(TestState(u32::from_le_bytes(raw)))
    }

    fn encode_delta(
        &self,
        delta: &TestDelta,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u32_le(delta.add)
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<TestDelta, ApplicationCodecError> {
        if self.fail_delta_decode {
            return Err(ApplicationCodecError::with_source(
                ApplicationCodecOperation::DecodeDelta,
                ApplicationCodecFailureKind::Decode,
                Some(Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "application delta codec io failure",
                ))),
            ));
        }
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("test delta must be four bytes"))?;
        Ok(TestDelta {
            add: u32::from_le_bytes(raw),
        })
    }
}

fn checkpoint_parts() -> (
    DeviceTable,
    RedBlackTreeMapSync<pi_block_device::BlockRef, RecordSeq>,
    RedBlackTreeMapSync<BindingName, pi_block_device::BlockRef>,
) {
    let registration = DeviceRegistration {
        id: BlockDeviceId::from_bytes([1; 16]).expect("device id is valid"),
        label: DeviceLabel::new("primary".to_owned()).expect("device label is valid"),
    };
    let devices = DeviceTable::new(vec![DeviceSlot::new(
        registration,
        BlockDeviceState::initial().expect("initial device state is valid"),
    )])
    .expect("device table is valid");
    (
        devices,
        RedBlackTreeMapSync::new_sync(),
        RedBlackTreeMapSync::new_sync(),
    )
}

async fn write_checkpoint_and_delta(
    path: std::path::PathBuf,
    max_payload_len: usize,
    checkpoint: Bytes,
    delta: Bytes,
) {
    let checkpoint_seq = RecordSeq::new(1).expect("checkpoint sequence is valid");
    let delta_seq = RecordSeq::new(2).expect("delta sequence is valid");
    let log = SuperblockAppendLog::open(path, max_payload_len)
        .await
        .expect("empty real append log must open");
    assert!(matches!(
        log.append_durable(checkpoint_seq, RecordKind::Checkpoint, checkpoint)
            .await,
        DurableAppendOutcome::Durable { record_seq, .. } if record_seq == checkpoint_seq
    ));
    assert!(matches!(
        log.append_durable(delta_seq, RecordKind::Delta, delta).await,
        DurableAppendOutcome::Durable { record_seq, .. } if record_seq == delta_seq
    ));
}

#[test]
fn build_recovering_preserves_structured_delta_codec_error_chain() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("application-delta-codec-error-log");
        let block_codec = DefaultBlockCodec::default();
        let max_payload_len = block_codec.max_payload_len();
        let checkpoint_seq = RecordSeq::new(1).expect("checkpoint sequence is valid");
        let delta_seq = RecordSeq::new(2).expect("delta sequence is valid");
        let (devices, roots, bindings) = checkpoint_parts();
        let encoding_codec = TestCodec {
            fail_delta_decode: false,
        };
        let checkpoint = encode_checkpoint(
            checkpoint_seq,
            &devices,
            &roots,
            &bindings,
            &TestState(10),
            &encoding_codec,
        )
        .expect("checkpoint must encode");
        let changes = vec![TransactionChange::Application(TestDelta { add: 5 })];
        let delta = encode_delta::<TestState, TestCodec>(delta_seq, &changes, &encoding_codec)
            .expect("delta envelope and body must encode");
        write_checkpoint_and_delta(path.clone(), max_payload_len, checkpoint, delta).await;

        let decoding_codec = TestCodec {
            fail_delta_decode: true,
        };
        let result = pi_superblock::build_recovering(path, max_payload_len, &decoding_codec).await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("codec decode failure must not return recovered state"),
        };
        let OpenError::RecoveryFailed { kind, context } = &error else {
            panic!("expected RecoveryFailed, got {error:?}");
        };
        assert_eq!(*kind, RecoveryFailureKind::Decode);
        assert_eq!(context.operation, RecoveryOperation::DecodeDelta);

        let superblock_error = context
            .source
            .downcast_ref::<pi_superblock::SuperblockError>()
            .expect("recovery context must retain the superblock codec wrapper");
        let codec_error = Error::source(superblock_error)
            .and_then(|source| source.downcast_ref::<ApplicationCodecError>())
            .expect("superblock error must retain ApplicationCodecError");
        assert_eq!(
            codec_error.operation,
            ApplicationCodecOperation::DecodeDelta
        );
        assert_eq!(codec_error.reason, ApplicationCodecFailureKind::Decode);
        let io_error = Error::source(codec_error)
            .and_then(|source| source.downcast_ref::<io::Error>())
            .expect("codec error must retain io source");
        assert_eq!(io_error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(io_error.to_string(), "application delta codec io failure");
    });
}

#[test]
fn build_recovering_rejects_second_application_change_without_partial_state() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("second-application-change-error-log");
        let block_codec = DefaultBlockCodec::default();
        let max_payload_len = block_codec.max_payload_len();
        let checkpoint_seq = RecordSeq::new(1).expect("checkpoint sequence is valid");
        let delta_seq = RecordSeq::new(2).expect("delta sequence is valid");
        let (devices, roots, bindings) = checkpoint_parts();
        let codec = TestCodec {
            fail_delta_decode: false,
        };
        let checkpoint = encode_checkpoint(
            checkpoint_seq,
            &devices,
            &roots,
            &bindings,
            &TestState(10),
            &codec,
        )
        .expect("checkpoint must encode");
        let changes = vec![
            TransactionChange::Application(TestDelta { add: 5 }),
            TransactionChange::Application(TestDelta { add: 7 }),
        ];
        let delta = encode_delta::<TestState, TestCodec>(delta_seq, &changes, &codec)
            .expect("two application changes must encode");
        write_checkpoint_and_delta(path.clone(), max_payload_len, checkpoint, delta).await;

        let result = pi_superblock::build_recovering(path, max_payload_len, &codec).await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("failed second change must not publish a partially recovered state"),
        };
        let OpenError::RecoveryFailed { kind, context } = &error else {
            panic!("expected RecoveryFailed, got {error:?}");
        };
        assert_eq!(*kind, RecoveryFailureKind::ApplicationReplay);
        assert_eq!(context.operation, RecoveryOperation::ReplayApplication);

        let apply_error = context
            .source
            .downcast_ref::<ApplicationApplyError>()
            .expect("recovery context must retain ApplicationApplyError");
        assert_eq!(apply_error.change_index, 1);
        let io_error = Error::source(apply_error)
            .and_then(|source| source.downcast_ref::<io::Error>())
            .expect("apply error must retain io source");
        assert_eq!(io_error.kind(), io::ErrorKind::Other);
        assert_eq!(io_error.to_string(), "second application change failed");
    });
}
