use super::*;
use futures_lite::future::block_on;
use pi_result::ClassifyErrorKind;
use std::future::{Future, ready};
use std::io;
use std::task::{Context, Poll, Waker};

#[test]
fn all_record_kinds_force_barrier_before_durable() {
    for configured in [false, true] {
        for (raw, kind) in [
            (1, crate::RecordKind::Checkpoint),
            (2, crate::RecordKind::Delta),
            (3, crate::RecordKind::Checkpoint),
        ] {
            let seq = RecordSeq::new(raw).unwrap();
            let encoded =
                encode_append_log_block(&DefaultBlockCodec::default(), seq, kind, b"body").unwrap();
            let outcome = block_on(append_confirmed(
                seq,
                encoded.clone(),
                AppendOptions {
                    durable: configured,
                },
                |received, options| {
                    assert_eq!(received, encoded);
                    assert!(
                        options.durable,
                        "initial/maintenance Checkpoint and Delta must sync"
                    );
                    ready(Ok(123))
                },
            ));
            assert!(
                matches!(outcome, DurableAppendOutcome::Durable { record_seq, active_size: 123 } if record_seq == seq)
            );
        }
    }
}

#[test]
fn pending_barrier_cannot_produce_durable_and_sync_error_is_unknown() {
    let seq = RecordSeq::new(1).unwrap();
    let mut future = Box::pin(append_confirmed(
        seq,
        vec![1],
        AppendOptions { durable: false },
        |_, options| {
            assert!(options.durable);
            std::future::pending::<pi_result::Result<u64>>()
        },
    ));
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    drop(future);
    let outcome = block_on(append_confirmed(
        seq,
        vec![1],
        AppendOptions { durable: false },
        |_, options| {
            assert!(options.durable);
            ready(
                Err(io::Error::other("injected sync failure"))
                    .into_error_by(|error| error.classify_error_kind()),
            )
        },
    ));
    let DurableAppendOutcome::Unknown { source } = outcome else {
        panic!("sync error must remain unknown")
    };
    let report = source
        .source()
        .unwrap()
        .downcast_ref::<PiResultError>()
        .unwrap();
    assert_eq!(
        report.error.classify_error_kind(),
        io::Error::other("injected sync failure").classify_error_kind()
    );
    assert!(format!("{:?}", report.error).contains("injected sync failure"));
}

#[test]
fn configured_false_real_checkpoint_delta_and_checkpoint_replay() {
    block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("log");
        let log = SuperblockAppendLog::open_configured(
            path.clone(),
            1024,
            AppendOptions { durable: false },
        )
        .await
        .unwrap();
        for (raw, kind) in [
            (1, crate::RecordKind::Checkpoint),
            (2, crate::RecordKind::Delta),
            (3, crate::RecordKind::Checkpoint),
        ] {
            assert!(matches!(
                log.append_durable(
                    RecordSeq::new(raw).unwrap(),
                    kind,
                    Bytes::from_static(b"body")
                )
                .await,
                DurableAppendOutcome::Durable { .. }
            ));
        }
        drop(log);
        let mut records = Vec::new();
        SuperblockAppendLog::open_replaying(path, 1024, |record| {
            records.push((record.record_seq.raw(), record.header.record_kind));
            false
        })
        .await
        .unwrap();
        assert_eq!(
            records,
            vec![
                (1, crate::RecordKind::Checkpoint),
                (2, crate::RecordKind::Delta),
                (3, crate::RecordKind::Checkpoint)
            ]
        );
    });
}
