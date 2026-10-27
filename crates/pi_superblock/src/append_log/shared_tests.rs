//! Shared adapter tests independent of unfinished SB/checkpoint/registry runtime.
use super::*;
use crate::registry::test_models::state;
use futures_lite::future::block_on;

#[test]
fn sequential_shared_appends_keep_confirmed_size_and_atomic_threshold() {
    block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let log = Arc::new(
            SuperblockAppendLog::open(temp.path().join("log"), 1024)
                .await
                .unwrap(),
        );
        log.set_rotate_threshold(1);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = Arc::clone(&log);
        let thread = std::thread::spawn(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            block_on(worker.append_durable(
                RecordSeq::new(2).unwrap(),
                crate::RecordKind::Delta,
                Bytes::from_static(b"second"),
            ))
        });
        entered_rx.recv().unwrap();
        let first = log
            .append_durable(
                RecordSeq::new(1).unwrap(),
                crate::RecordKind::Checkpoint,
                Bytes::from_static(b"first"),
            )
            .await;
        assert!(matches!(first, DurableAppendOutcome::Durable { .. }));
        release_tx.send(()).unwrap();
        let DurableAppendOutcome::Durable { active_size, .. } = thread.join().unwrap() else {
            panic!("durable append")
        };
        assert_eq!(log.active_size.load(Ordering::Acquire), active_size);
        assert_eq!(
            log.maintenance_phase(),
            LogMaintenancePhase::RotateRequested
        );
    });
}

#[test]
fn real_archive_is_idempotent_and_failed_round_retains_queue() {
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
        log.storage.archive(closed.clone()).await.unwrap();
        log.storage.archive(closed.clone()).await.unwrap();
        log.pending_archive
            .lock()
            .unwrap()
            .push_back(closed.clone());
        log.set_phase(LogMaintenancePhase::ArchivePending);
        assert!(matches!(
            log.run_archive(state()).await.unwrap(),
            MaintenanceOutcome::Published { archived: true, .. }
        ));
        assert!(log.pending_archive.lock().unwrap().is_empty());
        // A token owned by another real log is rejected without consuming responsibility.
        let other = SuperblockAppendLog::open(temp.path().join("other"), 1024)
            .await
            .unwrap();
        other.pending_archive.lock().unwrap().push_back(closed);
        other.set_phase(LogMaintenancePhase::ArchivePending);
        assert!(matches!(
            other.run_archive(state()).await.unwrap(),
            MaintenanceOutcome::ArchiveRetryPending { .. }
        ));
        assert_eq!(other.pending_archive.lock().unwrap().len(), 1);
        assert_eq!(other.maintenance_phase(), LogMaintenancePhase::ArchiveRetry);
    });
}

#[test]
fn checkpoint_required_and_recovery_fence_reject_delta_without_append() {
    block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let log = SuperblockAppendLog::open(temp.path().join("log"), 1024)
            .await
            .unwrap();
        for phase in [
            LogMaintenancePhase::CheckpointRequired,
            LogMaintenancePhase::RecoveryRequired,
        ] {
            log.set_phase(phase);
            assert!(matches!(
                log.append_durable(
                    RecordSeq::new(1).unwrap(),
                    crate::RecordKind::Delta,
                    Bytes::from_static(b"body")
                )
                .await,
                DurableAppendOutcome::NotAppended { .. }
            ));
            assert_eq!(log.active_size.load(Ordering::Acquire), 0);
        }
        log.set_phase(LogMaintenancePhase::Active);
        assert_eq!(
            log.maintenance_phase(),
            LogMaintenancePhase::RecoveryRequired
        );
    });
}
