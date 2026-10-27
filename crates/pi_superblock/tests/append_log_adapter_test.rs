use bytes::Bytes;
use pi_append_log::{BlockDecoder, BlockEncoder, DefaultBlockCodec};
#[test]
fn decoded_append_log_checkpoint_preserves_sequence_header_and_body() {
    let codec = DefaultBlockCodec::default();
    let header = pi_superblock::encode_record_header(pi_superblock::RecordKind::Checkpoint)
        .expect("Checkpoint header must encode");
    let expected_header =
        pi_superblock::decode_record_header(&header).expect("Checkpoint header must decode");
    let body = Bytes::from_static(b"checkpoint-body\x00\xffpreserved");
    let mut payload = Vec::from(header);
    payload.extend_from_slice(&body);

    let encoded = codec
        .encode(2, 0, &payload)
        .expect("flags=0 append-log block must encode");
    let decoded_block = codec
        .decode_forward(&encoded)
        .expect("encoded append-log block must strictly decode");

    assert_eq!(decoded_block.encoded(), encoded.as_slice());
    assert_eq!(decoded_block.block_seq(), 2);
    assert_eq!(decoded_block.flags(), 0);
    assert_eq!(decoded_block.payload(), payload.as_slice());
    assert_eq!(&decoded_block.payload()[..8], header.as_slice());

    let record = pi_superblock::decode_append_log_block(decoded_block)
        .expect("validated append-log block must adapt to a superblock record");

    assert_eq!(record.record_seq.raw(), 2);
    assert_eq!(record.header, expected_header);
    assert_eq!(record.body, body);
}

#[test]
fn decoded_append_log_header_only_payload_has_empty_body() {
    let codec = DefaultBlockCodec::default();
    let header = pi_superblock::encode_record_header(pi_superblock::RecordKind::Checkpoint)
        .expect("Checkpoint header must encode");
    let expected_header =
        pi_superblock::decode_record_header(&header).expect("Checkpoint header must decode");
    let encoded = codec
        .encode(2, 0, &header)
        .expect("header-only append-log block must encode");
    let decoded_block = codec
        .decode_forward(&encoded)
        .expect("header-only append-log block must strictly decode");

    let record = pi_superblock::decode_append_log_block(decoded_block)
        .expect("eight-byte header payload must adapt");

    assert_eq!(record.record_seq.raw(), 2);
    assert_eq!(record.header, expected_header);
    assert!(record.body.is_empty());
}

#[test]
fn decoded_append_log_payload_shorter_than_header_is_invalid_state() {
    let codec = DefaultBlockCodec::default();
    let payload = [0_u8; 7];
    let encoded = codec
        .encode(2, 0, &payload)
        .expect("short payload remains a valid append-log envelope");
    let decoded_block = codec
        .decode_forward(&encoded)
        .expect("short payload append-log block must strictly decode");

    let result = pi_superblock::decode_append_log_block(decoded_block);

    assert!(
        matches!(result, Err(pi_superblock::SuperblockError::InvalidState)),
        "payload shorter than the eight-byte superblock header must map to InvalidState: {result:?}"
    );
}

#[test]
fn decoded_append_log_u32_max_block_sequence_is_invalid_state() {
    let codec = DefaultBlockCodec::default();
    let header = pi_superblock::encode_record_header(pi_superblock::RecordKind::Checkpoint)
        .expect("Checkpoint header must encode");
    let encoded = codec
        .encode(u32::MAX as u64, 0, &header)
        .expect("u32::MAX remains a valid append-log block sequence");
    let decoded_block = codec
        .decode_forward(&encoded)
        .expect("u32::MAX append-log block must strictly decode");

    let result = pi_superblock::decode_append_log_block(decoded_block);

    assert!(
        matches!(result, Err(pi_superblock::SuperblockError::InvalidState)),
        "adapter must map exhausted u32::MAX block sequence to InvalidState: {result:?}"
    );
}

#[test]
fn encoded_checkpoint_uses_record_sequence_only_as_block_sequence() {
    let codec = DefaultBlockCodec::default();
    let record_seq = pi_block_device::RecordSeq::new(2).expect("record sequence 2 is valid");
    let body = [0xff, 0x00, 0x7e, 0x11, 0x00];

    let encoded = pi_superblock::encode_append_log_block(
        &codec,
        record_seq,
        pi_superblock::RecordKind::Checkpoint,
        &body,
    )
    .expect("checkpoint record must encode into an append-log block");
    let decoded = codec
        .decode_forward(&encoded)
        .expect("encoded append-log block must strictly decode");

    let expected_payload = [
        0x50, 0x49, 0x53, 0x42, // "PISB" magic
        0x01, 0x00, // system version 1, little-endian
        0x01, // Checkpoint kind
        0x00, // superblock header flags
        0xff, 0x00, 0x7e, 0x11, 0x00, // original body, byte-for-byte
    ];

    assert_eq!(decoded.block_seq(), 2);
    assert_eq!(decoded.flags(), 0);
    assert_eq!(decoded.payload(), expected_payload.as_slice());
    assert_eq!(&decoded.payload()[8..], body.as_slice());
}

#[test]
fn null_record_sequence_is_rejected_as_exhausted() {
    let codec = DefaultBlockCodec::default();

    let result = pi_superblock::encode_append_log_block(
        &codec,
        pi_block_device::RecordSeq::NULL,
        pi_superblock::RecordKind::Checkpoint,
        &[],
    );

    assert!(
        matches!(
            result,
            Err(pi_superblock::SuperblockError::RecordSeqExhausted)
        ),
        "NULL is lifecycle-only and must map to RecordSeqExhausted, got {result:?}"
    );
}

#[test]
fn encoded_payload_exactly_at_codec_maximum_succeeds() {
    const HEADER_LEN: usize = 8;
    let body = [0xff, 0x00, 0x7e];
    let codec = DefaultBlockCodec::new(HEADER_LEN + body.len());
    let record_seq = pi_block_device::RecordSeq::new(2).expect("record sequence 2 is valid");

    let encoded = pi_superblock::encode_append_log_block(
        &codec,
        record_seq,
        pi_superblock::RecordKind::Checkpoint,
        &body,
    )
    .expect("payload exactly at the codec maximum must encode");
    let decoded = codec
        .decode_forward(&encoded)
        .expect("maximum-sized encoded block must strictly decode");

    assert_eq!(decoded.payload().len(), codec.max_payload_len());
    assert_eq!(&decoded.payload()[HEADER_LEN..], body.as_slice());
}

#[test]
fn encoded_payload_one_byte_over_codec_maximum_is_record_too_large() {
    const HEADER_LEN: usize = 8;
    let codec = DefaultBlockCodec::new(HEADER_LEN + 3);
    let body = [0xff, 0x00, 0x7e, 0x11];
    let record_seq = pi_block_device::RecordSeq::new(2).expect("record sequence 2 is valid");

    let result = pi_superblock::encode_append_log_block(
        &codec,
        record_seq,
        pi_superblock::RecordKind::Checkpoint,
        &body,
    );

    assert!(
        matches!(result, Err(pi_superblock::SuperblockError::RecordTooLarge)),
        "payload one byte over the codec maximum must map to RecordTooLarge, got {result:?}"
    );
}
