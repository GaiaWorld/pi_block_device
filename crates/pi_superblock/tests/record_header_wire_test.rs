use bytes::Bytes;
use pi_block_device::RecordSeq;
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, DeviceLabel,
    DeviceRegistration, DeviceSlot, DeviceTable, SuperblockError, decode_checkpoint,
};

#[derive(Clone, Debug)]
struct TestState;

impl ApplicationState for TestState {
    type Delta = ();
}

struct TestCodec;

fn minimal_device_prefix() -> Vec<u8> {
    let table = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: pi_block_device::BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
            label: DeviceLabel::new("a".to_owned()).expect("valid label"),
        },
        pi_block_device::BlockDeviceState::initial().expect("initial state"),
    )])
    .expect("valid table");
    let encoded = pi_superblock::encode_checkpoint(
        RecordSeq::new(1).expect("sequence"),
        &table,
        &rpds::RedBlackTreeMapSync::new_sync(),
        &rpds::RedBlackTreeMapSync::new_sync(),
        &TestState,
        &TestCodec,
    )
    .expect("encode minimal checkpoint");
    encoded[..encoded.len() - 8].to_vec()
}

impl ApplicationCodec<TestState> for TestCodec {
    fn encode_state(
        &self,
        _state: &TestState,
        _output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
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

#[test]
fn checkpoint_decoder_collection_count_limit_is_rejected_before_entry_reads() {
    // The first four bytes of a checkpoint body are exactly the device_count field.
    // The protocol requires rejecting counts above 1,000,000 before reading or
    // allocating any device entry collection.
    let encoded_device_count = Bytes::from(1_000_001_u32.to_le_bytes().to_vec());
    let sequence = RecordSeq::new(1).expect("sequence is valid");

    let result =
        decode_checkpoint::<TestState, TestCodec>(sequence, encoded_device_count, &TestCodec);

    assert!(
        matches!(result, Err(SuperblockError::RecordTooLarge)),
        "device_count=1,000,001 must be rejected as RecordTooLarge before allocation: {result:?}"
    );
}

#[test]
fn checkpoint_decoder_root_count_limit_is_rejected_before_root_reads() {
    let mut body = minimal_device_prefix();
    body.extend_from_slice(&1_000_001_u32.to_le_bytes());
    let sequence = RecordSeq::new(1).expect("sequence is valid");

    let result = decode_checkpoint::<TestState, TestCodec>(sequence, Bytes::from(body), &TestCodec);

    assert!(
        matches!(result, Err(SuperblockError::RecordTooLarge)),
        "root_count=1,000,001 must be rejected as RecordTooLarge before allocation: {result:?}"
    );
}

#[test]
fn checkpoint_decoder_binding_count_limit_is_rejected_before_binding_reads() {
    let mut body = minimal_device_prefix();
    body.extend_from_slice(&0_u32.to_le_bytes());
    body.extend_from_slice(&1_000_001_u32.to_le_bytes());
    let sequence = RecordSeq::new(1).expect("sequence is valid");

    let result = decode_checkpoint::<TestState, TestCodec>(sequence, Bytes::from(body), &TestCodec);

    assert!(
        matches!(result, Err(SuperblockError::RecordTooLarge)),
        "binding_count=1,000,001 must be rejected as RecordTooLarge before allocation: {result:?}"
    );
}

#[test]
fn checkpoint_decoder_binding_name_length_limit_is_rejected_before_name_reads() {
    let mut body = minimal_device_prefix();
    body.extend_from_slice(&0_u32.to_le_bytes());
    body.extend_from_slice(&1_u32.to_le_bytes());
    body.extend_from_slice(&1_048_577_u32.to_le_bytes());
    let sequence = RecordSeq::new(1).expect("sequence is valid");

    let result = decode_checkpoint::<TestState, TestCodec>(sequence, Bytes::from(body), &TestCodec);

    assert!(
        matches!(result, Err(SuperblockError::RecordTooLarge)),
        "name_len=1,048,577 must be rejected as RecordTooLarge before allocation: {result:?}"
    );
}

#[test]
fn static_device_table_uses_checked_contiguous_device_key_slots() {
    let first = DeviceRegistration {
        id: pi_block_device::BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
        label: DeviceLabel::new("first".to_owned()).expect("valid label"),
    };
    let second = DeviceRegistration {
        id: pi_block_device::BlockDeviceId::from_bytes([2; 16]).expect("valid id"),
        label: DeviceLabel::new("second".to_owned()).expect("valid label"),
    };
    let table = DeviceTable::new(vec![
        DeviceSlot::new(
            first,
            pi_block_device::BlockDeviceState::initial().expect("state"),
        ),
        DeviceSlot::new(
            second,
            pi_block_device::BlockDeviceState::initial().expect("state"),
        ),
    ])
    .expect("valid static table");

    assert_eq!(table.len(), 2);
    assert_eq!(
        table
            .get(pi_block_device::DeviceKey::new(1).expect("key"))
            .unwrap()
            .registration()
            .id,
        pi_block_device::BlockDeviceId::from_bytes([1; 16]).expect("valid id")
    );
    assert_eq!(
        table
            .get(pi_block_device::DeviceKey::new(2).expect("key"))
            .unwrap()
            .registration()
            .id,
        pi_block_device::BlockDeviceId::from_bytes([2; 16]).expect("valid id")
    );
    assert!(
        table
            .get(pi_block_device::DeviceKey::new(3).expect("key"))
            .is_none()
    );
}

#[test]
fn static_device_table_rejects_duplicate_ids_and_labels() {
    let state = pi_block_device::BlockDeviceState::initial().expect("state");
    let id = pi_block_device::BlockDeviceId::from_bytes([1; 16]).expect("valid id");
    let first = DeviceSlot::new(
        DeviceRegistration {
            id,
            label: DeviceLabel::new("first".to_owned()).expect("valid label"),
        },
        state.clone(),
    );
    let duplicate_id = DeviceSlot::new(
        DeviceRegistration {
            id,
            label: DeviceLabel::new("second".to_owned()).expect("valid label"),
        },
        state.clone(),
    );
    assert!(DeviceTable::new(vec![first, duplicate_id]).is_err());

    let duplicate_label = "same".to_owned();
    let first = DeviceSlot::new(
        DeviceRegistration {
            id: pi_block_device::BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
            label: DeviceLabel::new(duplicate_label.clone()).expect("valid label"),
        },
        state.clone(),
    );
    let second = DeviceSlot::new(
        DeviceRegistration {
            id: pi_block_device::BlockDeviceId::from_bytes([2; 16]).expect("valid id"),
            label: DeviceLabel::new(duplicate_label).expect("valid label"),
        },
        state,
    );
    assert!(DeviceTable::new(vec![first, second]).is_err());
}

#[test]
fn checkpoint_decoder_rejects_noncontiguous_device_keys() {
    let table = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: pi_block_device::BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
            label: DeviceLabel::new("a".to_owned()).expect("valid label"),
        },
        pi_block_device::BlockDeviceState::initial().expect("initial state"),
    )])
    .expect("valid table");
    let mut encoded = pi_superblock::encode_checkpoint(
        RecordSeq::new(1).expect("sequence"),
        &table,
        &rpds::RedBlackTreeMapSync::new_sync(),
        &rpds::RedBlackTreeMapSync::new_sync(),
        &TestState,
        &TestCodec,
    )
    .expect("encode checkpoint")
    .to_vec();
    encoded[4..8].copy_from_slice(&2_u32.to_le_bytes());

    let result = decode_checkpoint::<TestState, TestCodec>(
        RecordSeq::new(1).expect("sequence"),
        Bytes::from(encoded),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::InvalidBlockRef)));
}

#[test]
fn checkpoint_decoder_rejects_application_remainder_above_state_limit() {
    let mut body = minimal_device_prefix();
    body.extend_from_slice(&0_u32.to_le_bytes());
    body.extend_from_slice(&0_u32.to_le_bytes());
    body.resize(body.len() + 64 * 1024 * 1024 + 1, 0);

    let result = decode_checkpoint::<TestState, TestCodec>(
        RecordSeq::new(1).expect("sequence"),
        Bytes::from(body),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::RecordTooLarge)));
}
