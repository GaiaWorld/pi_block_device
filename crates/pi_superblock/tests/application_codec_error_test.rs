use rpds::RedBlackTreeMapSync;
use std::{error::Error, io};

use bytes::Bytes;
use pi_block_device::{BlockDeviceId, BlockDeviceState, RecordSeq};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationCodecFailureKind,
    ApplicationCodecOperation, ApplicationEncoder, ApplicationState, BindingName, DeviceLabel,
    DeviceRegistration, DeviceSlot, DeviceTable, SuperblockError, decode_delta, encode_checkpoint,
};

const STATE_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState;

impl ApplicationState for TestState {
    type Delta = ();
}

struct OversizedStateCodec;

impl ApplicationCodec<TestState> for OversizedStateCodec {
    fn encode_state(
        &self,
        _state: &TestState,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_slice(&vec![0_u8; STATE_LIMIT + 1])
    }

    fn decode_state(&self, _bytes: Bytes) -> Result<TestState, ApplicationCodecError> {
        Ok(TestState)
    }

    fn encode_delta(
        &self,
        _delta: &(),
        _output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
    }

    fn decode_delta(&self, _bytes: Bytes) -> Result<(), ApplicationCodecError> {
        Ok(())
    }
}

struct StructuredFailureCodec {
    operation: ApplicationCodecOperation,
    reason: ApplicationCodecFailureKind,
    error_kind: io::ErrorKind,
    message: &'static str,
}

impl StructuredFailureCodec {
    fn error(&self) -> ApplicationCodecError {
        ApplicationCodecError {
            operation: self.operation,
            reason: self.reason,
            source: Some(Box::new(io::Error::new(self.error_kind, self.message))),
        }
    }
}

impl ApplicationCodec<TestState> for StructuredFailureCodec {
    fn encode_state(
        &self,
        _state: &TestState,
        _output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Err(self.error())
    }

    fn decode_state(&self, _bytes: Bytes) -> Result<TestState, ApplicationCodecError> {
        Ok(TestState)
    }

    fn encode_delta(
        &self,
        _delta: &(),
        _output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
    }

    fn decode_delta(&self, _bytes: Bytes) -> Result<(), ApplicationCodecError> {
        Err(self.error())
    }
}

fn valid_seq() -> RecordSeq {
    RecordSeq::new(1).expect("sequence 1 is valid")
}

fn minimal_devices() -> DeviceTable {
    let registration = DeviceRegistration {
        id: BlockDeviceId::from_bytes([1; 16]).expect("nonzero device id is valid"),
        label: DeviceLabel::new("primary".to_owned()).expect("device label is valid"),
    };
    DeviceTable::new(vec![DeviceSlot::new(
        registration,
        BlockDeviceState::initial().expect("initial device state is valid"),
    )])
    .expect("single-device table is valid")
}

fn empty_checkpoint_maps() -> (
    RedBlackTreeMapSync<pi_block_device::BlockRef, RecordSeq>,
    RedBlackTreeMapSync<BindingName, pi_block_device::BlockRef>,
) {
    (
        RedBlackTreeMapSync::new_sync(),
        RedBlackTreeMapSync::new_sync(),
    )
}

fn encoded_application_delta() -> Bytes {
    let mut bytes = Vec::with_capacity(12);
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&[2, 0]);
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    Bytes::from(bytes)
}

#[test]
fn checkpoint_state_single_write_over_limit_maps_to_record_too_large() {
    let devices = minimal_devices();
    let (roots, bindings) = empty_checkpoint_maps();

    let result = encode_checkpoint(
        valid_seq(),
        &devices,
        &roots,
        &bindings,
        &TestState,
        &OversizedStateCodec,
    );

    assert!(
        matches!(result, Err(SuperblockError::RecordTooLarge)),
        "one 64 MiB + 1 application state write must map to RecordTooLarge, got {result:?}"
    );
}

#[test]
fn delta_decode_preserves_structured_codec_error_and_io_source_chain() {
    const SOURCE_TEXT: &str = "structured delta decode failure";
    let codec = StructuredFailureCodec {
        operation: ApplicationCodecOperation::DecodeDelta,
        reason: ApplicationCodecFailureKind::Decode,
        error_kind: io::ErrorKind::InvalidData,
        message: SOURCE_TEXT,
    };

    let error =
        decode_delta::<TestState, StructuredFailureCodec>(encoded_application_delta(), &codec)
            .expect_err("codec decode error must be returned");

    let SuperblockError::ApplicationCodec { source } = &error else {
        panic!("expected ApplicationCodec, got {error:?}");
    };
    assert_eq!(source.operation, ApplicationCodecOperation::DecodeDelta);
    assert_eq!(source.reason, ApplicationCodecFailureKind::Decode);

    let application_source =
        Error::source(&error).expect("superblock error must expose codec source");
    let application_error = application_source
        .downcast_ref::<ApplicationCodecError>()
        .expect("first source must be ApplicationCodecError");
    let io_source = Error::source(application_error).expect("codec error must expose io source");
    let io_error = io_source
        .downcast_ref::<io::Error>()
        .expect("second source must be io::Error");
    assert_eq!(io_error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(io_error.to_string(), SOURCE_TEXT);
    assert!(Error::source(io_error).is_none());
}

#[test]
fn checkpoint_encode_state_preserves_structured_codec_error_and_source() {
    const SOURCE_TEXT: &str = "structured state encode failure";
    let devices = minimal_devices();
    let (roots, bindings) = empty_checkpoint_maps();
    let codec = StructuredFailureCodec {
        operation: ApplicationCodecOperation::EncodeState,
        reason: ApplicationCodecFailureKind::Encode,
        error_kind: io::ErrorKind::InvalidInput,
        message: SOURCE_TEXT,
    };

    let error = encode_checkpoint(valid_seq(), &devices, &roots, &bindings, &TestState, &codec)
        .expect_err("codec encode error must be returned");

    let SuperblockError::ApplicationCodec { source } = &error else {
        panic!("expected ApplicationCodec, got {error:?}");
    };
    assert_eq!(source.operation, ApplicationCodecOperation::EncodeState);
    assert_eq!(source.reason, ApplicationCodecFailureKind::Encode);

    let application_source =
        Error::source(&error).expect("superblock error must expose codec source");
    let application_error = application_source
        .downcast_ref::<ApplicationCodecError>()
        .expect("first source must be ApplicationCodecError");
    let io_source = Error::source(application_error).expect("codec error must expose io source");
    let io_error = io_source
        .downcast_ref::<io::Error>()
        .expect("second source must be io::Error");
    assert_eq!(io_error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(io_error.to_string(), SOURCE_TEXT);
    assert!(Error::source(io_error).is_none());
}
