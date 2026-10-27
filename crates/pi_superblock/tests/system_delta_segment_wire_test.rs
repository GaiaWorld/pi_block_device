use bytes::Bytes;
use pi_block_device::{BlockRange, DeviceKey, RecordSeq, SegmentVersion};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, SystemChange,
    TransactionChange, decode_delta, encode_delta,
};
use std::num::NonZeroU32;

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

    fn decode_state(&self, bytes: Bytes) -> Result<TestState, ApplicationCodecError> {
        if bytes.is_empty() {
            Ok(TestState)
        } else {
            Err(ApplicationCodecError::new(
                "test state payload must be empty",
            ))
        }
    }

    fn encode_delta(
        &self,
        _delta: &(),
        _output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<(), ApplicationCodecError> {
        if bytes.is_empty() {
            Ok(())
        } else {
            Err(ApplicationCodecError::new(
                "test delta payload must be empty",
            ))
        }
    }
}

fn device() -> DeviceKey {
    DeviceKey::new(2).expect("device 2 is valid")
}

fn version() -> SegmentVersion {
    SegmentVersion::new(7).expect("version 7 is valid")
}

fn assert_golden_roundtrip(change: SystemChange, expected: &'static [u8]) {
    let changes = vec![TransactionChange::System(change)];
    let encoded = encode_delta::<TestState, EmptyCodec>(
        RecordSeq::new(1).expect("sequence 1 is valid"),
        &changes,
        &EmptyCodec,
    )
    .expect("segment system change must encode");

    assert_eq!(encoded, Bytes::from_static(expected));
    assert_eq!(
        decode_delta::<TestState, EmptyCodec>(encoded, &EmptyCodec)
            .expect("golden segment change must decode"),
        changes
    );
}

#[test]
fn create_active_segment_tag_10_has_exact_payload_and_roundtrips() {
    assert_golden_roundtrip(
        SystemChange::CreateActiveSegment {
            device: device(),
            level: 3,
            version: version(),
        },
        &[
            0x01, 0x00, 0x00, 0x00, 0x01, 0x0a, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn allocate_segment_blocks_tag_11_has_exact_payload_and_roundtrips() {
    assert_golden_roundtrip(
        SystemChange::AllocateSegmentBlocks {
            device: device(),
            level: 3,
            version: version(),
            len: 9,
        },
        &[
            0x01, 0x00, 0x00, 0x00, 0x01, 0x0b, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn release_segment_blocks_tag_12_has_exact_ordered_ranges_and_roundtrips() {
    assert_golden_roundtrip(
        SystemChange::ReleaseSegmentBlocks {
            device: device(),
            level: 3,
            version: version(),
            ranges: vec![
                BlockRange {
                    start: 0,
                    len: NonZeroU32::new(2).expect("range length is nonzero"),
                },
                BlockRange {
                    start: 4,
                    len: NonZeroU32::new(3).expect("range length is nonzero"),
                },
            ],
        },
        &[
            0x01, 0x00, 0x00, 0x00, 0x01, 0x0c, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x00,
        ],
    );
}

#[test]
fn remove_segment_tag_13_has_exact_payload_and_roundtrips() {
    assert_golden_roundtrip(
        SystemChange::RemoveSegment {
            device: device(),
            level: 3,
            version: version(),
        },
        &[
            0x01, 0x00, 0x00, 0x00, 0x01, 0x0d, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00,
        ],
    );
}
