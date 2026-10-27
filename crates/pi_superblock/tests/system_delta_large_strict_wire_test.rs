use bytes::Bytes;
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, SuperblockError,
    decode_delta,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState;

impl ApplicationState for TestState {
    type Delta = ();
}

struct EmptyCodec;

impl ApplicationCodec<TestState> for EmptyCodec {
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

fn wire(tag: u8, payload: &[u8]) -> Bytes {
    let mut bytes = Vec::with_capacity(12 + payload.len());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.push(1);
    bytes.push(tag);
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    Bytes::from(bytes)
}

fn large_block_payload(device: u32, id: u64, logical_len: Option<u64>) -> Vec<u8> {
    let mut payload = Vec::with_capacity(if logical_len.is_some() { 20 } else { 12 });
    payload.extend_from_slice(&device.to_le_bytes());
    payload.extend_from_slice(&id.to_le_bytes());
    if let Some(logical_len) = logical_len {
        payload.extend_from_slice(&logical_len.to_le_bytes());
    }
    payload
}

fn assert_invalid(bytes: Bytes) {
    assert!(matches!(
        decode_delta::<TestState, EmptyCodec>(bytes, &EmptyCodec),
        Err(SuperblockError::InvalidState)
    ));
}

#[test]
fn tag_20_rejects_zero_logical_len_from_handwritten_wire() {
    let payload = large_block_payload(1, 1, Some(0));
    assert_eq!(payload.len(), 20);
    assert_invalid(wire(20, &payload));
}

#[test]
fn tag_20_rejects_short_payload() {
    let payload = large_block_payload(1, 1, Some(1));
    assert_invalid(wire(20, &payload[..19]));
}

#[test]
fn tag_20_rejects_trailing_payload_byte() {
    let mut payload = large_block_payload(1, 1, Some(1));
    payload.push(0);
    assert_invalid(wire(20, &payload));
}

#[test]
fn tag_21_rejects_short_payload() {
    let payload = large_block_payload(1, 1, None);
    assert_invalid(wire(21, &payload[..11]));
}

#[test]
fn tag_21_rejects_trailing_payload_byte() {
    let mut payload = large_block_payload(1, 1, None);
    payload.push(0);
    assert_invalid(wire(21, &payload));
}

#[test]
fn tag_22_rejects_short_payload() {
    let payload = large_block_payload(1, 1, None);
    assert_invalid(wire(22, &payload[..11]));
}

#[test]
fn tag_22_rejects_trailing_payload_byte() {
    let mut payload = large_block_payload(1, 1, None);
    payload.push(0);
    assert_invalid(wire(22, &payload));
}

#[test]
fn large_block_tags_reject_zero_device() {
    for tag in [20, 21, 22] {
        let payload = large_block_payload(0, 1, (tag == 20).then_some(1));
        assert_invalid(wire(tag, &payload));
    }
}

#[test]
fn large_block_tags_reject_zero_id() {
    for tag in [20, 21, 22] {
        let payload = large_block_payload(1, 0, (tag == 20).then_some(1));
        assert_invalid(wire(tag, &payload));
    }
}

#[test]
fn large_block_tags_reject_reserved_sentinel_id() {
    for tag in [20, 21, 22] {
        let payload = large_block_payload(1, (1_u64 << 60) - 1, (tag == 20).then_some(1));
        assert_invalid(wire(tag, &payload));
    }
}
