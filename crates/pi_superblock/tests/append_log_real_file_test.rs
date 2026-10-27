extern crate pi_result;

use bytes::Bytes;
use futures_lite::future::block_on;
use pi_append_log::file::{DefaultFileLayout, FileAppendLogBuilder};
use pi_append_log::{
    AppendLogBuilder, AppendLogVisitor, BlockDecoder, BlockVisitContext, DefaultBlockCodec,
    ReadOrder,
};
use pi_superblock::{
    DecodedSuperblockRecord, DurableAppendOutcome, RecordKind, SuperblockAppendLog,
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
fn durable_checkpoint_append_is_recovered_from_the_real_file_log() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("superblock-log");
        assert!(path.is_absolute(), "append-log path must be absolute");

        let codec = DefaultBlockCodec::default();
        let record_seq =
            pi_block_device::RecordSeq::new(7).expect("record sequence 7 must be valid");
        let kind = RecordKind::Checkpoint;
        let body = Bytes::from_static(b"checkpoint-body\x00\xffdurable");

        let adapter = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("a real file append log must open");
        let outcome = adapter.append_durable(record_seq, kind, body.clone()).await;

        match outcome {
            DurableAppendOutcome::Durable {
                record_seq: actual_record_seq,
                active_size,
            } => {
                assert_eq!(actual_record_seq, record_seq);
                assert!(
                    active_size > 0,
                    "durable append must report a nonzero active size"
                );
            }
            other => panic!("checkpoint append must be durable, got {other:?}"),
        }
        drop(adapter);

        let mut visitor = Forward::default();
        let rebuilt = FileAppendLogBuilder::new(&path, DefaultFileLayout::new(&path))
            .build(&codec, ReadOrder::Forward, &mut visitor)
            .await
            .expect("the durable file append log must rebuild in Forward order");
        drop(rebuilt.storage);

        assert_eq!(
            visitor.records.len(),
            1,
            "the rebuilt log must contain exactly one record"
        );
        let recovered = &visitor.records[0];
        assert_eq!(recovered.record_seq, record_seq);
        assert_eq!(recovered.header.record_kind, kind);
        assert_eq!(recovered.body, body);
    });
}

#[test]
fn open_recovers_durable_history_in_forward_order_and_remains_appendable() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("recoverable-superblock-log");
        let codec = DefaultBlockCodec::default();

        let seq1 = pi_block_device::RecordSeq::new(1).expect("record sequence 1 must be valid");
        let seq2 = pi_block_device::RecordSeq::new(2).expect("record sequence 2 must be valid");
        let seq3 = pi_block_device::RecordSeq::new(3).expect("record sequence 3 must be valid");
        let body_a = Bytes::from_static(b"bodyA");
        let body_b = Bytes::from_static(b"bodyB");

        let initial = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("an empty real file append log must open");

        assert!(
            matches!(
                initial
                    .append_durable(seq1, RecordKind::Checkpoint, body_a.clone())
                    .await,
                DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq1
            ),
            "sequence 1 checkpoint must be durable"
        );
        assert!(
            matches!(
                initial
                    .append_durable(seq2, RecordKind::Delta, body_b.clone())
                    .await,
                DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq2
            ),
            "sequence 2 delta must be durable"
        );
        drop(initial);

        let mut recovered = Vec::new();
        let reopened =
            SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |record| {
                recovered.push(record);
                false
            })
            .await
            .expect("open must recover valid durable history instead of returning InvalidState");

        assert_eq!(recovered.len(), 2, "both durable records must be recovered");
        assert_eq!(recovered[0].record_seq, seq1);
        assert_eq!(recovered[0].header.record_kind, RecordKind::Checkpoint);
        assert_eq!(recovered[0].body, body_a);
        assert_eq!(recovered[1].record_seq, seq2);
        assert_eq!(recovered[1].header.record_kind, RecordKind::Delta);
        assert_eq!(recovered[1].body, body_b);

        assert!(
            matches!(
                reopened
                    .append_durable(seq3, RecordKind::Delta, Bytes::from_static(b"bodyC"))
                    .await,
                DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq3
            ),
            "the storage returned by recovery must remain appendable"
        );
    });
}

#[test]
fn open_replaying_accepts_sequence_gap_and_remains_appendable() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("sequence-gap-superblock-log");
        let codec = DefaultBlockCodec::default();

        let seq1 = pi_block_device::RecordSeq::new(1).expect("record sequence 1 must be valid");
        let seq3 = pi_block_device::RecordSeq::new(3).expect("record sequence 3 must be valid");
        let seq4 = pi_block_device::RecordSeq::new(4).expect("record sequence 4 must be valid");

        let initial = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("an empty real file append log must open");
        for (record_seq, kind, body) in [
            (
                seq1,
                RecordKind::Checkpoint,
                Bytes::from_static(b"checkpoint-seq-1"),
            ),
            (
                seq3,
                RecordKind::Delta,
                Bytes::from_static(b"delta-seq-3-after-gap"),
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
                "each strictly increasing record must be durably arranged"
            );
        }
        drop(initial);

        let mut recovered_sequences = Vec::new();
        let reopened =
            SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |record| {
                recovered_sequences.push(record.record_seq);
                false
            })
            .await
            .expect("a strictly increasing sequence gap must replay successfully");

        assert_eq!(
            recovered_sequences,
            vec![seq1, seq3],
            "replay callback must receive gapped records in forward order"
        );
        assert!(
            matches!(
                reopened
                    .append_durable(seq4, RecordKind::Delta, Bytes::from_static(b"delta-seq-4"))
                    .await,
                DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq4
            ),
            "the adapter returned after gapped replay must remain appendable"
        );
    });
}

#[test]
fn open_replaying_stops_after_callback_returns_true_and_remains_appendable() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("callback-stop-superblock-log");
        let codec = DefaultBlockCodec::default();

        let seq1 = pi_block_device::RecordSeq::new(1).expect("record sequence 1 must be valid");
        let seq2 = pi_block_device::RecordSeq::new(2).expect("record sequence 2 must be valid");
        let seq3 = pi_block_device::RecordSeq::new(3).expect("record sequence 3 must be valid");
        let seq4 = pi_block_device::RecordSeq::new(4).expect("record sequence 4 must be valid");

        let initial = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("an empty real file append log must open");
        for (record_seq, kind, body) in [
            (seq1, RecordKind::Checkpoint, Bytes::from_static(b"body-1")),
            (seq2, RecordKind::Delta, Bytes::from_static(b"body-2")),
            (seq3, RecordKind::Delta, Bytes::from_static(b"body-3")),
        ] {
            assert!(
                matches!(
                    initial
                        .append_durable(record_seq, kind, body)
                        .await,
                    DurableAppendOutcome::Durable {
                        record_seq: actual,
                        ..
                    } if actual == record_seq
                ),
                "each arranged record must be durable"
            );
        }
        drop(initial);

        let mut callback_count = 0;
        let mut recovered_sequences = Vec::new();
        let reopened =
            SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |record| {
                callback_count += 1;
                recovered_sequences.push(record.record_seq);
                true
            })
            .await
            .expect("callback true must stop replay without preventing builder initialization");

        assert_eq!(
            callback_count, 1,
            "callback true on the first record must stop further replay"
        );
        assert_eq!(
            recovered_sequences,
            vec![seq1],
            "forward replay must deliver only the oldest record before stopping"
        );
        assert!(
            matches!(
                reopened
                    .append_durable(seq4, RecordKind::Delta, Bytes::from_static(b"body-4"))
                    .await,
                DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq4
            ),
            "the adapter returned after stopped replay must remain appendable"
        );
    });
}
