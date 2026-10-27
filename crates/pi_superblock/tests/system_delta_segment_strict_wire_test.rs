use std::num::NonZeroU32;

use bytes::Bytes;
use pi_block_device::{BlockRange, DeviceKey, RecordSeq, SegmentVersion};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, SuperblockError,
    SystemChange, TransactionChange, decode_delta, encode_delta,
};

const COLLECTION_LIMIT: u32 = 1_000_000;

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

fn segment_payload(device: u32, level: u8, reserved: [u8; 3], version: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(12);
    payload.extend_from_slice(&device.to_le_bytes());
    payload.push(level);
    payload.extend_from_slice(&reserved);
    payload.extend_from_slice(&version.to_le_bytes());
    payload
}

fn release_payload(ranges: &[(u32, u32)]) -> Vec<u8> {
    let mut payload = segment_payload(2, 3, [0; 3], 7);
    payload.extend_from_slice(&(ranges.len() as u32).to_le_bytes());
    for (start, len) in ranges {
        payload.extend_from_slice(&start.to_le_bytes());
        payload.extend_from_slice(&len.to_le_bytes());
    }
    payload
}

fn decode(bytes: Bytes) -> Result<Vec<TransactionChange<()>>, SuperblockError> {
    decode_delta::<TestState, EmptyCodec>(bytes, &EmptyCodec)
}

fn assert_invalid(bytes: Bytes) {
    assert!(matches!(decode(bytes), Err(SuperblockError::InvalidState)));
}

fn assert_too_large(bytes: Bytes) {
    assert!(matches!(
        decode(bytes),
        Err(SuperblockError::RecordTooLarge)
    ));
}

fn encode(change: SystemChange) -> Result<Bytes, SuperblockError> {
    encode_delta::<TestState, EmptyCodec>(
        RecordSeq::new(1).expect("record sequence is valid"),
        &[TransactionChange::System(change)],
        &EmptyCodec,
    )
}

fn valid_device() -> DeviceKey {
    DeviceKey::new(2).expect("device is valid")
}

fn valid_version() -> SegmentVersion {
    SegmentVersion::new(7).expect("version is valid")
}

fn range(start: u32, len: u32) -> BlockRange {
    BlockRange {
        start,
        len: NonZeroU32::new(len).expect("test range length must be nonzero"),
    }
}

#[test]
fn tag_10_rejects_short_payload() {
    let payload = segment_payload(2, 3, [0; 3], 7);
    assert_invalid(wire(10, &payload[..11]));
}

#[test]
fn tag_10_rejects_trailing_payload_byte() {
    let mut payload = segment_payload(2, 3, [0; 3], 7);
    payload.push(0);
    assert_invalid(wire(10, &payload));
}

#[test]
fn tag_11_rejects_short_payload() {
    let mut payload = segment_payload(2, 3, [0; 3], 7);
    payload.extend_from_slice(&9_u32.to_le_bytes());
    assert_invalid(wire(11, &payload[..15]));
}

#[test]
fn tag_11_rejects_trailing_payload_byte() {
    let mut payload = segment_payload(2, 3, [0; 3], 7);
    payload.extend_from_slice(&9_u32.to_le_bytes());
    payload.push(0);
    assert_invalid(wire(11, &payload));
}

#[test]
fn tag_13_rejects_short_payload() {
    let payload = segment_payload(2, 3, [0; 3], 7);
    assert_invalid(wire(13, &payload[..11]));
}

#[test]
fn tag_13_rejects_trailing_payload_byte() {
    let mut payload = segment_payload(2, 3, [0; 3], 7);
    payload.push(0);
    assert_invalid(wire(13, &payload));
}

#[test]
fn segment_header_rejects_nonzero_reserved_bytes() {
    for tag in [10, 13] {
        assert_invalid(wire(tag, &segment_payload(2, 3, [1, 0, 0], 7)));
    }
    let mut allocate = segment_payload(2, 3, [0, 1, 0], 7);
    allocate.extend_from_slice(&1_u32.to_le_bytes());
    assert_invalid(wire(11, &allocate));
}

#[test]
fn segment_header_rejects_device_zero() {
    assert_invalid(wire(10, &segment_payload(0, 3, [0; 3], 7)));
}

#[test]
fn segment_header_rejects_large_level_15() {
    assert_invalid(wire(10, &segment_payload(2, 15, [0; 3], 7)));
}

#[test]
fn segment_header_rejects_version_zero() {
    assert_invalid(wire(10, &segment_payload(2, 3, [0; 3], 0)));
}

#[test]
fn segment_header_rejects_version_above_value_type_maximum() {
    assert_invalid(wire(10, &segment_payload(2, 3, [0; 3], 0x1000_0000)));
}

#[test]
fn tag_11_rejects_zero_allocation_length() {
    let mut payload = segment_payload(2, 3, [0; 3], 7);
    payload.extend_from_slice(&0_u32.to_le_bytes());
    assert_invalid(wire(11, &payload));
}

#[test]
fn tag_12_rejects_count_above_collection_limit_before_payload_allocation() {
    let mut payload = segment_payload(2, 3, [0; 3], 7);
    payload.extend_from_slice(&(COLLECTION_LIMIT + 1).to_le_bytes());
    assert_too_large(wire(12, &payload));
}

#[test]
fn tag_12_rejects_truncated_payload_at_collection_limit() {
    let mut payload = segment_payload(2, 3, [0; 3], 7);
    payload.extend_from_slice(&COLLECTION_LIMIT.to_le_bytes());
    assert_invalid(wire(12, &payload));
}

#[test]
fn tag_12_rejects_zero_range_length() {
    assert_invalid(wire(12, &release_payload(&[(4, 0)])));
}

#[test]
fn tag_12_rejects_range_end_overflow() {
    assert_invalid(wire(12, &release_payload(&[(u32::MAX, 1)])));
}

#[test]
fn tag_12_rejects_ranges_with_same_start() {
    assert_invalid(wire(12, &release_payload(&[(4, 1), (4, 2)])));
}

#[test]
fn tag_12_rejects_descending_ranges() {
    assert_invalid(wire(12, &release_payload(&[(8, 1), (4, 1)])));
}

#[test]
fn tag_12_rejects_overlapping_ranges() {
    assert_invalid(wire(12, &release_payload(&[(4, 4), (7, 2)])));
}

#[test]
fn tag_12_accepts_empty_ranges() {
    let decoded = decode(wire(12, &release_payload(&[]))).expect("empty ranges are valid");
    assert_eq!(
        decoded,
        vec![TransactionChange::System(
            SystemChange::ReleaseSegmentBlocks {
                device: valid_device(),
                level: 3,
                version: valid_version(),
                ranges: vec![],
            }
        )]
    );
}

#[test]
fn tag_12_accepts_adjacent_ranges() {
    let decoded =
        decode(wire(12, &release_payload(&[(4, 3), (7, 2)]))).expect("adjacent ranges are valid");
    assert_eq!(
        decoded,
        vec![TransactionChange::System(
            SystemChange::ReleaseSegmentBlocks {
                device: valid_device(),
                level: 3,
                version: valid_version(),
                ranges: vec![range(4, 3), range(7, 2)],
            }
        )]
    );
}

#[test]
fn encoder_rejects_ranges_with_same_start() {
    let result = encode(SystemChange::ReleaseSegmentBlocks {
        device: valid_device(),
        level: 3,
        version: valid_version(),
        ranges: vec![range(4, 1), range(4, 2)],
    });
    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn encoder_rejects_descending_ranges() {
    let result = encode(SystemChange::ReleaseSegmentBlocks {
        device: valid_device(),
        level: 3,
        version: valid_version(),
        ranges: vec![range(8, 1), range(4, 1)],
    });
    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn encoder_rejects_overlapping_ranges() {
    let result = encode(SystemChange::ReleaseSegmentBlocks {
        device: valid_device(),
        level: 3,
        version: valid_version(),
        ranges: vec![range(4, 4), range(7, 2)],
    });
    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn encoder_rejects_range_end_overflow() {
    let result = encode(SystemChange::ReleaseSegmentBlocks {
        device: valid_device(),
        level: 3,
        version: valid_version(),
        ranges: vec![range(u32::MAX, 1)],
    });
    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

// Encoder-side device zero, version zero/overflow, and range length zero fixtures cannot be
// constructed through the public API: DeviceKey, SegmentVersion, and BlockRange::len enforce
// those invariants in their value types. No invalid values are forged here.
