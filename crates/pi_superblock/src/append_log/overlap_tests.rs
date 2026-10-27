//! Regression at the public shared archive entry, with real file rename and no sleeps.
use super::*;
use crate::registry::test_models::state;
use futures_lite::future::block_on;
use std::future::Future;
use std::task::{Context, Poll, Waker};

#[test]
fn overlapping_archive_waits_then_consumes_cancelled_real_archive_responsibility() {
    block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let log = SuperblockAppendLog::open(temp.path().join("log"), 1024)
            .await
            .unwrap();
        assert!(matches!(
            log.append_durable(
                RecordSeq::new(1).unwrap(),
                crate::RecordKind::Checkpoint,
                Bytes::from_static(b"body")
            )
            .await,
            DurableAppendOutcome::Durable { .. }
        ));
        let closed = log.storage.rotate().await.unwrap().unwrap();
        log.pending_archive
            .lock()
            .unwrap()
            .push_back(closed.clone());
        log.set_phase(LogMaintenancePhase::ArchivePending);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        *log.archive_completed_hook.lock().unwrap() = Some(entered_tx);
        let mut first = Box::pin(log.run_archive(state()));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(first.as_mut().poll(&mut cx), Poll::Pending));
        // Drive actual namespace rename to completion; hook then deliberately parks A.
        while entered_rx.try_recv().is_err() {
            assert!(matches!(first.as_mut().poll(&mut cx), Poll::Pending));
            std::thread::yield_now();
        }
        assert!(log.pending_archive.lock().unwrap().is_empty());
        let mut second = Box::pin(log.run_archive(state()));
        let second_poll = second.as_mut().poll(&mut cx);
        drop(first);
        assert_eq!(log.pending_archive.lock().unwrap().len(), 1);
        assert!(
            matches!(second_poll, Poll::Pending),
            "B must wait while A owns responsibility; not set Active on empty queue"
        );
        assert!(matches!(
            second.await.unwrap(),
            MaintenanceOutcome::Published { archived: true, .. }
        ));
        assert!(log.pending_archive.lock().unwrap().is_empty());
        assert_eq!(log.maintenance_phase(), LogMaintenancePhase::Active);
        // Dependency observes ArchiveOnly and succeeds again; no fixture archive implementation.
        log.storage.archive(closed).await.unwrap();
    });
}
