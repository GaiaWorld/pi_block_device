use bytes::Bytes;
use pi_block_device::RecordSeq;
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState,
    TransactionChange, decode_delta, encode_delta,
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
        delta: &u32,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_slice(&delta.to_le_bytes())
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<u32, ApplicationCodecError> {
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("test delta must be exactly four bytes"))?;
        Ok(u32::from_le_bytes(raw))
    }
}

#[test]
fn application_delta_wire_has_golden_envelopes_and_omits_record_seq() {
    let changes = vec![
        TransactionChange::Application(0x1122_3344),
        TransactionChange::Application(0x5566_7788),
    ];
    let expected = Bytes::from_static(&[
        0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x44, 0x33, 0x22,
        0x11, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x88, 0x77, 0x66, 0x55,
    ]);

    let encoded_at_seq_1 = encode_delta::<TestState, TestCodec>(
        RecordSeq::new(1).expect("sequence 1 is valid"),
        &changes,
        &TestCodec,
    )
    .expect("application deltas must encode");
    let encoded_at_seq_7 = encode_delta::<TestState, TestCodec>(
        RecordSeq::new(7).expect("sequence 7 is valid"),
        &changes,
        &TestCodec,
    )
    .expect("application deltas must encode");

    assert_eq!(encoded_at_seq_1, expected);
    assert_eq!(encoded_at_seq_7, expected);
    assert_eq!(encoded_at_seq_1, encoded_at_seq_7);

    let decoded = decode_delta::<TestState, TestCodec>(encoded_at_seq_1, &TestCodec)
        .expect("golden application delta body must decode");
    assert_eq!(decoded, changes);
}

#[test]
fn empty_application_delta_body_is_zero_count_and_roundtrips() {
    let changes: Vec<TransactionChange<u32>> = Vec::new();

    let encoded = encode_delta::<TestState, TestCodec>(
        RecordSeq::new(1).expect("sequence is valid"),
        &changes,
        &TestCodec,
    )
    .expect("empty delta list must encode");

    assert_eq!(encoded, Bytes::from_static(&[0x00, 0x00, 0x00, 0x00]));

    let decoded = decode_delta::<TestState, TestCodec>(encoded, &TestCodec)
        .expect("zero-count delta body must decode");
    assert_eq!(decoded, changes);
}
