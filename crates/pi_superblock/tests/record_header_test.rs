use pi_superblock::{decode_record_header, encode_record_header};

#[test]
fn checkpoint_header_matches_frozen_eight_byte_layout() {
    let encoded =
        encode_record_header(pi_superblock::RecordKind::Checkpoint).expect("valid header");
    assert_eq!(encoded, [b'P', b'I', b'S', b'B', 1, 0, 1, 0]);
    let decoded = decode_record_header(&encoded).expect("roundtrip header");
    assert_eq!(decoded.record_kind, pi_superblock::RecordKind::Checkpoint);
    assert_eq!(decoded.flags, 0);
}

#[test]
fn decoder_rejects_nonzero_flags_and_unknown_kind() {
    let mut encoded = encode_record_header(pi_superblock::RecordKind::Delta).expect("valid header");
    encoded[7] = 1;
    assert!(decode_record_header(&encoded).is_err());
    encoded[7] = 0;
    encoded[6] = 3;
    assert!(decode_record_header(&encoded).is_err());
}

#[test]
fn decoder_rejects_damaged_magic() {
    let mut encoded =
        encode_record_header(pi_superblock::RecordKind::Checkpoint).expect("valid header");
    encoded[0] ^= 1;
    assert!(decode_record_header(&encoded).is_err());
}

#[test]
fn decoder_rejects_invalid_system_version() {
    let mut encoded =
        encode_record_header(pi_superblock::RecordKind::Checkpoint).expect("valid header");
    encoded[4] = 2;
    assert!(decode_record_header(&encoded).is_err());
}

#[test]
fn decoder_rejects_truncated_or_extended_header() {
    let encoded =
        encode_record_header(pi_superblock::RecordKind::Checkpoint).expect("valid header");
    assert!(decode_record_header(&encoded[..7]).is_err());
    let mut extended = encoded.to_vec();
    extended.push(0);
    assert!(decode_record_header(&extended).is_err());
}

#[test]
fn append_log_block_sequence_maps_to_record_sequence_with_strict_bounds() {
    assert_eq!(
        pi_superblock::record_seq_from_block_seq(0),
        Err(pi_superblock::InvalidBlockSequence)
    );
    assert_eq!(
        pi_superblock::record_seq_from_block_seq(1)
            .expect("first sequence")
            .raw(),
        1
    );
    assert_eq!(
        pi_superblock::record_seq_from_block_seq(u32::MAX as u64 - 1)
            .expect("last legal sequence")
            .raw(),
        u32::MAX - 1,
    );
    assert_eq!(
        pi_superblock::record_seq_from_block_seq(u32::MAX as u64),
        Err(pi_superblock::InvalidBlockSequence)
    );
    assert_eq!(
        pi_superblock::record_seq_from_block_seq(u32::MAX as u64 + 1),
        Err(pi_superblock::InvalidBlockSequence)
    );
}
