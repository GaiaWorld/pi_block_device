//! Private ownership/cancellation checks; no timer or unfinished checkpoint codec.
use super::*;
use futures_lite::future::block_on;
use pi_block_device::RecordSeq;
use std::future::Future;
use std::task::{Context, Poll, Waker};

#[test]
fn cancelled_archive_round_returns_owned_responsibility_without_io() {
    block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let log = SuperblockAppendLog::open(temp.path().join("log"), 1024)
            .await
            .unwrap();
        log.append_durable(
            RecordSeq::new(1).unwrap(),
            crate::RecordKind::Checkpoint,
            bytes::Bytes::from_static(b"body"),
        )
        .await;
        let closed = log.storage.rotate().await.unwrap().unwrap();
        log.pending_archive.lock().unwrap().push_back(closed);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let mut future = Box::pin(async {
            let remaining = std::mem::take(&mut *log.pending_archive.lock().unwrap());
            let _round = ArchiveRound {
                queue: &log.pending_archive,
                remaining,
            };
            entered_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
        entered_rx.recv().unwrap();
        assert!(log.pending_archive.lock().unwrap().is_empty());
        drop(future);
        assert_eq!(log.pending_archive.lock().unwrap().len(), 1);
        log.set_phase(LogMaintenancePhase::ArchivePending);
        assert!(matches!(
            log.run_archive(crate::registry::test_models::state())
                .await
                .unwrap(),
            MaintenanceOutcome::Published { archived: true, .. }
        ));
    });
}

#[test]
fn cancelled_maintenance_attempt_fences_log_before_next_append() {
    block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let log = SuperblockAppendLog::open(temp.path().join("log"), 1024)
            .await
            .unwrap();
        let mut future = Box::pin(async {
            let _attempt = MaintenanceAttempt {
                log: &log,
                armed: true,
            };
            log.set_phase(LogMaintenancePhase::Rotating);
            std::future::pending::<()>().await;
        });
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(future.as_mut().poll(&mut cx), Poll::Pending));
        drop(future);
        assert_eq!(
            log.maintenance_phase(),
            LogMaintenancePhase::RecoveryRequired
        );
        assert!(matches!(
            log.append_durable(
                RecordSeq::new(1).unwrap(),
                crate::RecordKind::Checkpoint,
                bytes::Bytes::from_static(b"body")
            )
            .await,
            crate::DurableAppendOutcome::NotAppended { .. }
        ));
    });
}
