use bytes::Bytes;
use pi_block_device::{BlockId, BlockRef, DeviceKey, RecordSeq, SegmentVersion};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, BindingName,
    SuperblockError, SystemChange, TransactionChange, decode_delta, encode_delta,
};

const FROZEN_BLOCK_REF_BYTES: [u8; 12] = [
    0x01, 0x00, 0x00, 0x00, // DeviceKey raw u32 = 1, little-endian.
    0x10, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00,
    0x00, // BlockId raw u64 = 0x0000000700000010, little-endian.
];

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
        output.put_slice(&state.0.to_le_bytes())
    }

    fn decode_state(&self, bytes: Bytes) -> Result<TestState, ApplicationCodecError> {
        Ok(TestState(decode_exact_u32(bytes)?))
    }

    fn encode_delta(
        &self,
        delta: &u32,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_slice(&delta.to_le_bytes())
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<u32, ApplicationCodecError> {
        decode_exact_u32(bytes)
    }
}

fn decode_exact_u32(bytes: Bytes) -> Result<u32, ApplicationCodecError> {
    let raw: [u8; 4] = bytes
        .as_ref()
        .try_into()
        .map_err(|_| ApplicationCodecError::new("test codec requires exactly four bytes"))?;
    Ok(u32::from_le_bytes(raw))
}

fn valid_seq() -> RecordSeq {
    RecordSeq::new(1).expect("sequence 1 is valid")
}

fn valid_block() -> BlockRef {
    BlockRef {
        device: DeviceKey::new(1).expect("device key 1 is valid"),
        block: BlockId::ordinary(0, SegmentVersion::new(1).expect("version 1 is valid"), 7)
            .expect("ordinary block is valid"),
    }
}

fn binding_name(bytes: &[u8]) -> BindingName {
    BindingName::new(bytes.to_vec()).expect("fixture binding name is valid")
}

fn envelope(transaction_tag: u8, change_tag: u8, flags: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.push(transaction_tag);
    bytes.push(change_tag);
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(payload.len())
            .expect("fixture payload length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(payload);
    bytes
}

fn system_envelope(change_tag: u8, payload: &[u8]) -> Vec<u8> {
    envelope(1, change_tag, 0, payload)
}

fn envelope_with_declared_payload_len(
    transaction_tag: u8,
    change_tag: u8,
    flags: u16,
    payload_len: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.push(transaction_tag);
    bytes.push(change_tag);
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn delta_body(envelopes: &[Vec<u8>]) -> Bytes {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        &u32::try_from(envelopes.len())
            .expect("fixture count fits u32")
            .to_le_bytes(),
    );
    for item in envelopes {
        bytes.extend_from_slice(item);
    }
    Bytes::from(bytes)
}

fn assert_golden_roundtrip(change: TransactionChange<u32>, expected: Bytes) {
    let changes = vec![change];
    let encoded = encode_delta::<TestState, TestCodec>(valid_seq(), &changes, &TestCodec)
        .expect("supported system change must encode");

    assert_eq!(encoded, expected);
    assert_eq!(
        decode_delta::<TestState, TestCodec>(encoded, &TestCodec)
            .expect("golden system change must decode"),
        changes
    );
}

#[test]
fn add_root_freezes_tag_30_with_exact_block_ref_payload_and_roundtrips() {
    let block = valid_block();
    let expected = delta_body(&[system_envelope(30, &FROZEN_BLOCK_REF_BYTES)]);

    assert_golden_roundtrip(
        TransactionChange::System(SystemChange::AddRoot { block }),
        expected,
    );
}

#[test]
fn remove_root_freezes_tag_31_with_exact_block_ref_payload_and_roundtrips() {
    let block = valid_block();
    let expected = delta_body(&[system_envelope(31, &FROZEN_BLOCK_REF_BYTES)]);

    assert_golden_roundtrip(
        TransactionChange::System(SystemChange::RemoveRoot { block }),
        expected,
    );
}

#[test]
fn add_binding_freezes_tag_32_with_original_utf8_name_and_block_ref_and_roundtrips() {
    let block = valid_block();
    let name_bytes = "根-α".as_bytes();
    let name = binding_name(name_bytes);
    let mut payload = Vec::new();
    payload.extend_from_slice(
        &u32::try_from(name_bytes.len())
            .expect("name length fits u32")
            .to_le_bytes(),
    );
    payload.extend_from_slice(name_bytes);
    payload.extend_from_slice(&FROZEN_BLOCK_REF_BYTES);
    let expected = delta_body(&[system_envelope(32, &payload)]);

    assert_golden_roundtrip(
        TransactionChange::System(SystemChange::AddBinding { name, block }),
        expected,
    );
}

#[test]
fn remove_binding_freezes_tag_33_with_exact_name_payload_and_roundtrips() {
    let name_bytes = b"primary";
    let name = binding_name(name_bytes);
    let mut payload = Vec::new();
    payload.extend_from_slice(
        &u32::try_from(name_bytes.len())
            .expect("name length fits u32")
            .to_le_bytes(),
    );
    payload.extend_from_slice(name_bytes);
    let expected = delta_body(&[system_envelope(33, &payload)]);

    assert_golden_roundtrip(
        TransactionChange::System(SystemChange::RemoveBinding { name }),
        expected,
    );
}

#[test]
fn mixed_system_application_system_changes_preserve_original_wire_order() {
    let block = valid_block();
    let name_bytes = "绑定".as_bytes();
    let name = binding_name(name_bytes);
    let changes = vec![
        TransactionChange::System(SystemChange::AddRoot { block }),
        TransactionChange::Application(0x1122_3344),
        TransactionChange::System(SystemChange::AddBinding { name, block }),
    ];
    let mut binding_payload = Vec::new();
    binding_payload.extend_from_slice(
        &u32::try_from(name_bytes.len())
            .expect("name length fits u32")
            .to_le_bytes(),
    );
    binding_payload.extend_from_slice(name_bytes);
    binding_payload.extend_from_slice(&FROZEN_BLOCK_REF_BYTES);
    let expected = delta_body(&[
        system_envelope(30, &FROZEN_BLOCK_REF_BYTES),
        envelope(2, 0, 0, &[0x44, 0x33, 0x22, 0x11]),
        system_envelope(32, &binding_payload),
    ]);

    let encoded = encode_delta::<TestState, TestCodec>(valid_seq(), &changes, &TestCodec)
        .expect("mixed transaction changes must encode");
    assert_eq!(encoded, expected);
    assert_eq!(
        decode_delta::<TestState, TestCodec>(encoded, &TestCodec)
            .expect("mixed transaction changes must decode"),
        changes
    );
}

#[test]
fn decode_rejects_unknown_system_change_tag_as_invalid_state() {
    let result =
        decode_delta::<TestState, TestCodec>(delta_body(&[system_envelope(34, &[])]), &TestCodec);

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_nonzero_system_flags_as_invalid_state() {
    let result = decode_delta::<TestState, TestCodec>(
        delta_body(&[envelope(1, 30, 1, &FROZEN_BLOCK_REF_BYTES)]),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_tag_30_with_11_byte_payload_as_invalid_state() {
    let result = decode_delta::<TestState, TestCodec>(
        delta_body(&[system_envelope(30, &FROZEN_BLOCK_REF_BYTES[..11])]),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_tag_30_with_13_byte_payload_as_invalid_state() {
    let mut payload = FROZEN_BLOCK_REF_BYTES.to_vec();
    payload.push(0xff);

    let result = decode_delta::<TestState, TestCodec>(
        delta_body(&[system_envelope(30, &payload)]),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_tag_32_with_zero_name_len_as_invalid_state() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0_u32.to_le_bytes());
    payload.push(0);
    payload.extend_from_slice(&FROZEN_BLOCK_REF_BYTES);

    let result = decode_delta::<TestState, TestCodec>(
        delta_body(&[system_envelope(32, &payload)]),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_tag_32_with_invalid_utf8_name_as_invalid_state() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1_u32.to_le_bytes());
    payload.push(0xff);
    payload.extend_from_slice(&FROZEN_BLOCK_REF_BYTES);

    let result = decode_delta::<TestState, TestCodec>(
        delta_body(&[system_envelope(32, &payload)]),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_oversized_binding_name_before_reading_name_payload() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1_048_577_u32.to_le_bytes());
    payload.push(0);

    let result = decode_delta::<TestState, TestCodec>(
        delta_body(&[system_envelope(33, &payload)]),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::RecordTooLarge)));
}

#[test]
fn decode_rejects_declared_payload_len_larger_than_available_as_invalid_state() {
    let envelope = envelope_with_declared_payload_len(1, 30, 0, 12, &FROZEN_BLOCK_REF_BYTES[..11]);
    let result = decode_delta::<TestState, TestCodec>(delta_body(&[envelope]), &TestCodec);

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_tag_33_payload_trailing_byte_as_invalid_state() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1_u32.to_le_bytes());
    payload.push(b'a');
    payload.push(0xff);

    let result = decode_delta::<TestState, TestCodec>(
        delta_body(&[system_envelope(33, &payload)]),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}
