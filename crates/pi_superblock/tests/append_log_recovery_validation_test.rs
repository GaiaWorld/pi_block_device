extern crate pi_result;

use bytes::Bytes;
use futures_lite::future::block_on;
use pi_append_log::file::{DefaultFileLayout, FileAppendLogBuilder};
use pi_append_log::{
    AppendLog, AppendLogBuilder, AppendLogVisitor, AppendOptions, BlockDecoder, BlockEncoder,
    BlockVisitContext, DefaultBlockCodec, ReadOrder,
};
use pi_superblock::{
    DecodedSuperblockRecord, DurableAppendOutcome, OpenError, RecordKind, RecoveryFailureKind,
    RecoveryOperation, SuperblockAppendLog,
};
use tempfile::tempdir;

#[derive(Default)]
struct Forward {
    records: Vec<DecodedSuperblockRecord>,
}

impl AppendLogVisitor for Forward {
    fn visit(&mut self, block: &[u8], _context: BlockVisitContext) -> pi_result::Result<bool> {
        let codec = DefaultBlockCodec::default();
        let decoded_block = codec
            .decode_forward(block)
            .expect("visitor must strictly decode the complete append-log block");
        assert_eq!(
            decoded_block.encoded_len(),
            block.len(),
            "visitor must reject trailing bytes after a decoded block"
        );
        let record = pi_superblock::decode_append_log_block(decoded_block)
            .expect("visitor must strictly decode the superblock envelope");
        self.records.push(record);
        Ok(false)
    }
}

#[test]
fn open_classifies_crc_valid_superblock_header_decode_failure_as_decode() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("invalid-superblock-header-log");
        let codec = DefaultBlockCodec::default();

        let mut payload = Vec::from(*b"BAD!\x01\x00\x01\x00");
        payload.extend_from_slice(b"complete-body");
        let encoded = codec
            .encode(1, 0, &payload)
            .expect("append-log codec must produce a complete CRC-valid frame");

        let mut visitor = Forward::default();
        let build = FileAppendLogBuilder::new(&path, DefaultFileLayout::new(&path))
            .build(&codec, ReadOrder::Forward, &mut visitor)
            .await
            .expect("real file append-log storage must be created");
        assert!(visitor.records.is_empty(), "new append log must be empty");

        build
            .storage
            .append(encoded, AppendOptions { durable: true })
            .await
            .expect("complete CRC-valid frame must be durably appended");
        drop(build.storage);

        let result =
            SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |_record| false)
                .await;

        match result {
            Err(OpenError::RecoveryFailed { kind, context }) => {
                assert_eq!(
                    kind,
                    RecoveryFailureKind::Decode,
                    "a CRC-valid frame with an invalid superblock header is a decode failure"
                );
                assert_eq!(context.operation, RecoveryOperation::OpenLog);
                assert_eq!(context.device_key, None);
                assert!(
                    !context.source.to_string().trim().is_empty(),
                    "decode failure must retain non-empty source text"
                );
            }
            Err(other) => panic!("expected structured recovery decode failure, got {other:?}"),
            Ok(_) => panic!("invalid superblock header must fail recovery"),
        }
    });
}

#[test]
fn open_classifies_crc_valid_higher_system_version_as_unsupported_format() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("unsupported-superblock-version-log");
        let codec = DefaultBlockCodec::default();

        let payload = [
            0x50, 0x49, 0x53, 0x42, // "PISB" magic
            0x02, 0x00, // unsupported system version 2, little-endian
            0x01, // Checkpoint kind
            0x00, // superblock header flags
        ];
        let encoded = codec
            .encode(1, 0, &payload)
            .expect("append-log codec must produce a complete CRC-valid frame");

        let mut visitor = Forward::default();
        let build = FileAppendLogBuilder::new(&path, DefaultFileLayout::new(&path))
            .build(&codec, ReadOrder::Forward, &mut visitor)
            .await
            .expect("real file append-log storage must be created");
        assert!(visitor.records.is_empty(), "new append log must be empty");

        build
            .storage
            .append(encoded, AppendOptions { durable: true })
            .await
            .expect("complete CRC-valid frame must be durably appended");
        drop(build.storage);

        let result =
            SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |_record| false)
                .await;

        match result {
            Err(OpenError::RecoveryFailed { kind, context }) => {
                assert_eq!(
                    kind,
                    RecoveryFailureKind::UnsupportedFormat,
                    "a CRC-valid PISB frame above the current system version is unsupported"
                );
                assert_eq!(context.operation, RecoveryOperation::OpenLog);
                assert_eq!(context.device_key, None);
                assert!(
                    !context.source.to_string().trim().is_empty(),
                    "unsupported-format failure must retain non-empty source text"
                );
            }
            Err(other) => {
                panic!("expected structured unsupported-format recovery failure, got {other:?}")
            }
            Ok(_) => panic!("unsupported superblock system version must fail recovery"),
        }
    });
}

#[test]
fn open_replaying_rejects_delta_as_first_complete_record_in_active_structure() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("delta-first-active-structure-log");
        let codec = DefaultBlockCodec::default();
        let seq1 = pi_block_device::RecordSeq::new(1).expect("record sequence 1 must be valid");

        let initial = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("an empty real file append log must open");
        assert!(
            matches!(
                initial
                    .append_durable(
                        seq1,
                        RecordKind::Delta,
                        Bytes::from_static(b"delta-without-checkpoint"),
                    )
                    .await,
                DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq1
            ),
            "the first complete active-structure record must be durably arranged"
        );
        drop(initial);

        let mut callback_count = 0;
        let result =
            SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |_record| {
                callback_count += 1;
                false
            })
            .await;

        assert_eq!(
            callback_count, 0,
            "a leading Delta must be rejected before invoking the callback"
        );
        match result {
            Err(OpenError::RecoveryFailed { kind, context }) => {
                assert_eq!(
                    kind,
                    RecoveryFailureKind::Decode,
                    "a Delta as the active structure's first complete record is invalid"
                );
                assert_eq!(context.operation, RecoveryOperation::OpenLog);
                assert_eq!(context.device_key, None);
                assert!(
                    !context.source.to_string().trim().is_empty(),
                    "decode failure must retain non-empty source text"
                );
            }
            Err(other) => panic!("expected structured recovery decode failure, got {other:?}"),
            Ok(_) => panic!("a Delta cannot start the current active structure"),
        }
    });
}

#[test]
fn open_replaying_rejects_duplicate_sequence_in_active_structure() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("duplicate-sequence-active-structure-log");
        let codec = DefaultBlockCodec::default();
        let seq2 = pi_block_device::RecordSeq::new(2).expect("record sequence 2 must be valid");

        let initial = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("an empty real file append log must open");
        for (kind, body) in [
            (
                RecordKind::Checkpoint,
                Bytes::from_static(b"checkpoint-seq-2"),
            ),
            (
                RecordKind::Delta,
                Bytes::from_static(b"duplicate-delta-seq-2"),
            ),
        ] {
            assert!(
                matches!(
                    initial.append_durable(seq2, kind, body).await,
                    DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq2
                ),
                "both duplicate-sequence records must be durably arranged"
            );
        }
        drop(initial);

        let mut recovered_sequences = Vec::new();
        let result = SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |record| {
            recovered_sequences.push(record.record_seq);
            false
        })
        .await;

        assert_eq!(
            recovered_sequences,
            vec![seq2],
            "replay may deliver the first checkpoint but must reject the duplicate sequence before invoking the callback"
        );
        match result {
            Err(OpenError::RecoveryFailed { kind, context }) => {
                assert_eq!(
                    kind,
                    RecoveryFailureKind::Decode,
                    "a duplicate sequence in one active structure is a decode failure"
                );
                assert_eq!(context.operation, RecoveryOperation::OpenLog);
                assert_eq!(context.device_key, None);
            }
            Err(other) => panic!("expected structured recovery decode failure, got {other:?}"),
            Ok(_) => panic!("a duplicate sequence must fail recovery"),
        }
    });
}

#[test]
fn open_replaying_rejects_decreasing_sequence_in_active_structure() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("decreasing-sequence-active-structure-log");
        let codec = DefaultBlockCodec::default();
        let seq2 = pi_block_device::RecordSeq::new(2).expect("record sequence 2 must be valid");
        let seq3 = pi_block_device::RecordSeq::new(3).expect("record sequence 3 must be valid");

        let initial = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("an empty real file append log must open");
        for (record_seq, kind, body) in [
            (
                seq3,
                RecordKind::Checkpoint,
                Bytes::from_static(b"checkpoint-seq-3"),
            ),
            (
                seq2,
                RecordKind::Delta,
                Bytes::from_static(b"decreasing-delta-seq-2"),
            ),
        ] {
            assert!(
                matches!(
                    initial.append_durable(record_seq, kind, body).await,
                    DurableAppendOutcome::Durable {
                        record_seq: actual,
                        ..
                    } if actual == record_seq
                ),
                "both decreasing-sequence records must be durably arranged"
            );
        }
        drop(initial);

        let mut recovered_sequences = Vec::new();
        let result = SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |record| {
            recovered_sequences.push(record.record_seq);
            false
        })
        .await;

        assert_eq!(
            recovered_sequences,
            vec![seq3],
            "replay may deliver the checkpoint but must reject the decreasing sequence before invoking the callback"
        );
        match result {
            Err(OpenError::RecoveryFailed { kind, context }) => {
                assert_eq!(
                    kind,
                    RecoveryFailureKind::Decode,
                    "a decreasing sequence in one active structure is a decode failure"
                );
                assert_eq!(context.operation, RecoveryOperation::OpenLog);
                assert_eq!(context.device_key, None);
            }
            Err(other) => panic!("expected structured recovery decode failure, got {other:?}"),
            Ok(_) => panic!("a decreasing sequence must fail recovery"),
        }
    });
}

#[test]
fn open_replaying_callback_stop_still_validates_duplicate_sequence_suffix() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp
            .path()
            .join("callback-stop-duplicate-sequence-suffix-log");
        let codec = DefaultBlockCodec::default();
        let seq1 = pi_block_device::RecordSeq::new(1).expect("record sequence 1 must be valid");

        let initial = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("an empty real file append log must open");
        for (kind, body) in [
            (
                RecordKind::Checkpoint,
                Bytes::from_static(b"checkpoint-seq-1"),
            ),
            (
                RecordKind::Delta,
                Bytes::from_static(b"duplicate-delta-seq-1"),
            ),
        ] {
            assert!(
                matches!(
                    initial.append_durable(seq1, kind, body).await,
                    DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq1
                ),
                "both duplicate-sequence records must be durably arranged"
            );
        }
        drop(initial);

        let mut recovered_sequences = Vec::new();
        let result = SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |record| {
            recovered_sequences.push(record.record_seq);
            true
        })
        .await;

        assert_eq!(
            recovered_sequences,
            vec![seq1],
            "callback true must stop delivery after the first record"
        );
        match result {
            Err(OpenError::RecoveryFailed { kind, context }) => {
                assert_eq!(
                    kind,
                    RecoveryFailureKind::Decode,
                    "callback stop must not skip duplicate-sequence validation in the suffix"
                );
                assert_eq!(context.operation, RecoveryOperation::OpenLog);
                assert_eq!(context.device_key, None);
            }
            Err(other) => panic!("expected structured recovery decode failure, got {other:?}"),
            Ok(_) => panic!(
                "callback true may stop delivery but must not stop internal superblock validation"
            ),
        }
    });
}

#[test]
fn open_replaying_returns_not_initialized_for_missing_log_without_callback() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("missing-superblock-log");
        assert!(
            !path.exists(),
            "recovery precondition requires a log path that does not exist"
        );
        let codec = DefaultBlockCodec::default();
        let mut callback_count = 0;

        let result =
            SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |_record| {
                callback_count += 1;
                false
            })
            .await;

        assert_eq!(
            callback_count, 0,
            "recovery of a missing log must not invoke the replay callback"
        );
        assert!(
            matches!(result, Err(OpenError::NotInitialized)),
            "recovery of a missing log must return OpenError::NotInitialized"
        );
    });
}
