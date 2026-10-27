use bytes::{BufMut, Bytes};
use pi_block_device::RecordSeq;
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, SuperblockError,
    decode_checkpoint,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState;

impl ApplicationState for TestState {
    type Delta = u32;
}

struct TestCodec;

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
        _delta: &u32,
        _output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
    }

    fn decode_delta(&self, _bytes: Bytes) -> Result<u32, ApplicationCodecError> {
        Ok(0)
    }
}

#[test]
fn decode_checkpoint_rejects_large_collection_before_reading_entries() {
    let mut bytes = Vec::new();
    bytes.put_u32_le(1);
    bytes.put_u32_le(1);
    bytes.extend_from_slice(&[1; 16]);
    bytes.put_u16_le(1);
    bytes.put_u8(b'a');

    for _ in 0..15 {
        bytes.put_u32_le(1);
        bytes.put_u32_le(1);
        bytes.put_u8(1);
        bytes.put_u32_le(0);
        bytes.put_u32_le(0);
    }

    bytes.put_u64_le(1);
    bytes.put_u32_le(1_000_001);
    bytes.put_u32_le(0);
    bytes.put_u32_le(0);
    bytes.put_u32_le(0);

    let result = decode_checkpoint::<TestState, TestCodec>(
        RecordSeq::new(1).expect("valid sequence"),
        Bytes::from(bytes),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::RecordTooLarge)));
}
