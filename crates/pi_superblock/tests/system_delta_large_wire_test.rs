use bytes::Bytes;
use pi_block_device::{DeviceKey, LargeBlockId, RecordSeq};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, SystemChange,
    TransactionChange, decode_delta, encode_delta,
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

fn device() -> DeviceKey {
    DeviceKey::new(2).expect("device 2 is valid")
}

fn large_block_id() -> LargeBlockId {
    LargeBlockId::new(7).expect("large block id 7 is valid")
}

fn assert_golden_roundtrip(change: SystemChange, expected: &'static [u8]) {
    let changes = vec![TransactionChange::System(change)];
    let result = encode_delta::<TestState, EmptyCodec>(
        RecordSeq::new(1).expect("sequence 1 is valid"),
        &changes,
        &EmptyCodec,
    );

    assert!(
        result.is_ok(),
        "large-block system change must encode: {result:?}"
    );
    let encoded = result.expect("encoding was asserted successful");
    assert_eq!(encoded.as_ref(), expected);
    assert_eq!(
        decode_delta::<TestState, EmptyCodec>(Bytes::copy_from_slice(expected), &EmptyCodec)
            .expect("large-block golden bytes must decode"),
        changes
    );
}

#[test]
fn allocate_large_block_tag_20_has_exact_payload_and_roundtrips() {
    assert_golden_roundtrip(
        SystemChange::AllocateLargeBlock {
            device: device(),
            id: large_block_id(),
            logical_len: 524_289,
        },
        &[
            0x01, 0x00, 0x00, 0x00, 0x01, 0x14, 0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x08, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn release_large_block_tag_21_has_exact_payload_and_roundtrips() {
    assert_golden_roundtrip(
        SystemChange::ReleaseLargeBlock {
            device: device(),
            id: large_block_id(),
        },
        &[
            0x01, 0x00, 0x00, 0x00, 0x01, 0x15, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
    );
}

#[test]
fn remove_large_block_tag_22_has_exact_payload_and_roundtrips() {
    assert_golden_roundtrip(
        SystemChange::RemoveLargeBlock {
            device: device(),
            id: large_block_id(),
        },
        &[
            0x01, 0x00, 0x00, 0x00, 0x01, 0x16, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
    );
}
