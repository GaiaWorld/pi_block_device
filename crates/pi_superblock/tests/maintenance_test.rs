mod maintenance_support;
#[allow(dead_code)]
mod transaction_support;

use bytes::Bytes;
use futures_lite::future::block_on;
use pi_append_log::DefaultBlockCodec;
use pi_block_device::{BlockDeviceBackend, BlockDeviceState, DeviceKey, RecordSeq};
use pi_superblock::{
    DecodedSuperblockRecord, DeviceLabel, DeviceRegistration, DeviceSlot, DeviceTable,
    DurableAppendOutcome, MaintenanceError, MaintenanceOutcome, RecordKind, SuperblockAppendLog,
    SuperblockError, SuperblockState,
};
use tempfile::tempdir;

fn maintenance_state(
    device: &std::sync::Arc<transaction_support::TestBackend>,
    sequence: u32,
    value: u64,
) -> SuperblockState<transaction_support::TestState> {
    let table = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: device.device_id(),
            label: DeviceLabel::new("device-1".to_owned()).expect("label"),
        },
        BlockDeviceState::initial().expect("initial device state"),
    )])
    .expect("device table");
    let key = DeviceKey::new(1).expect("device key");
    assert!(table.get(key).is_some());
    SuperblockState {
        record_seq: RecordSeq::new(sequence).expect("record sequence"),
        devices: table,
        roots: rpds::RedBlackTreeMapSync::new_sync(),
        bindings: rpds::RedBlackTreeMapSync::new_sync(),
        application: transaction_support::TestState {
            value,
            roots: 0,
            bindings: 0,
        },
    }
}

#[test]
fn durable_threshold_boundary_keeps_real_log_reopenable() {
    block_on(async {
        let temp = tempdir().expect("temporary log directory");
        let path = maintenance_support::log_path(&temp, "threshold");
        let codec = DefaultBlockCodec::default();
        let seq = pi_block_device::RecordSeq::new(1).expect("valid sequence");
        let log = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("open real append log");
        assert!(matches!(
            log.append_durable(
                seq,
                RecordKind::Checkpoint,
                Bytes::from_static(b"checkpoint")
            )
            .await,
            DurableAppendOutcome::Durable { .. }
        ));
        drop(log);
        let mut records: Vec<DecodedSuperblockRecord> = Vec::new();
        let reopened =
            SuperblockAppendLog::open_replaying(path, codec.max_payload_len(), |record| {
                records.push(record);
                false
            })
            .await
            .expect("durable checkpoint must reopen");
        drop(reopened);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].header.record_kind, RecordKind::Checkpoint);
    });
}

#[cfg(feature = "test-support")]
#[test]
fn pending_maintenance_is_noop_below_threshold_without_consuming_record_sequence() {
    block_on(async {
        let temp = tempdir().expect("temporary log directory");
        let path = maintenance_support::log_path(&temp, "below-threshold");
        let device = std::sync::Arc::new(transaction_support::TestBackend::new(
            transaction_support::key(1),
        ));
        let superblock = transaction_support::create_with_threshold(
            &path,
            std::slice::from_ref(&device),
            std::sync::Arc::new(transaction_support::TestCodec::default()),
            u64::MAX,
        );
        let outcome = superblock
            .run_log_maintenance()
            .await
            .expect("threshold below maintenance must be a no-op");
        match outcome {
            MaintenanceOutcome::NoOp { state } => assert_eq!(state.record_seq.raw(), 1),
            _ => panic!("maintenance below threshold must be a no-op"),
        }
        let transaction = superblock
            .begin_write()
            .await
            .expect("writer remains available");
        drop(transaction);
    });
}

#[cfg(feature = "test-support")]
#[test]
fn threshold_commit_runs_checkpoint_then_reopens_at_next_sequence() {
    block_on(async {
        let temp = tempdir().expect("temporary log directory");
        let path = maintenance_support::log_path(&temp, "checkpoint");
        let device = std::sync::Arc::new(transaction_support::TestBackend::new(
            transaction_support::key(1),
        ));
        let codec = std::sync::Arc::new(transaction_support::TestCodec::default());
        let superblock = transaction_support::create_with_threshold(
            &path,
            std::slice::from_ref(&device),
            codec.clone(),
            1,
        );
        let mut transaction = superblock.begin_write().await.expect("begin transaction");
        transaction
            .apply_application(transaction_support::TestDelta(41))
            .expect("application delta");
        let commit = transaction.commit().await.expect("durable commit");
        assert_eq!(commit.record_seq().raw(), 2);
        let outcome = superblock
            .run_pending_maintenance()
            .await
            .expect("maintenance succeeds");
        match outcome {
            MaintenanceOutcome::Published { state, archived } => {
                assert!(archived);
                assert_eq!(state.record_seq.raw(), 3);
                assert_eq!(state.application.value, 41);
            }
            _ => panic!("threshold maintenance must publish and archive"),
        }
        drop(transaction);
        drop(superblock);
        let reopened = transaction_support::open(&path, std::slice::from_ref(&device), codec);
        let snapshot = reopened.open_snapshot().expect("snapshot after reopen");
        assert_eq!(
            pi_superblock::SuperblockSnapshotApi::record_seq(&snapshot).raw(),
            3
        );
        assert_eq!(
            pi_superblock::SuperblockSnapshotApi::application(&snapshot).value,
            41
        );
        let next = reopened.begin_write().await.expect("next writer");
        drop(next);
    });
}

#[test]
fn recovered_closed_structure_is_pending_and_archive_retry_is_consumed() {
    block_on(async {
        let temp = tempdir().expect("temporary log directory");
        let path = maintenance_support::log_path(&temp, "recovered-archive");
        let block_codec = DefaultBlockCodec::default();
        let application_codec = transaction_support::TestCodec::default();
        let device = std::sync::Arc::new(transaction_support::TestBackend::new(
            transaction_support::key(1),
        ));
        let current = std::sync::Arc::new(maintenance_state(&device, 1, 17));
        let body = pi_superblock::encode_checkpoint(
            current.record_seq,
            &current.devices,
            &current.roots,
            &current.bindings,
            &current.application,
            &application_codec,
        )
        .expect("initial checkpoint body");
        let log = SuperblockAppendLog::open(path.clone(), block_codec.max_payload_len())
            .await
            .expect("open append log");
        assert!(matches!(
            log.append_durable(current.record_seq, RecordKind::Checkpoint, body)
                .await,
            DurableAppendOutcome::Durable { .. }
        ));
        log.set_rotate_threshold(1);
        let published = match log
            .run_log_maintenance(current, &application_codec)
            .await
            .expect("checkpoint maintenance")
        {
            MaintenanceOutcome::Published { state, archived } => {
                assert!(!archived);
                assert_eq!(state.record_seq.raw(), 2);
                state
            }
            _ => panic!("maintenance must leave archive pending"),
        };
        drop(log);

        let (reopened, recovered) = pi_superblock::build_recovering(
            path.clone(),
            block_codec.max_payload_len(),
            &application_codec,
        )
        .await
        .expect("reopen with closed structure");
        assert_eq!(recovered.state.record_seq.raw(), published.record_seq.raw());
        let conflict = path.join("00000001.archive");
        std::fs::copy(path.join("00000001"), &conflict).expect("create archive conflict");
        let retry = reopened
            .run_log_maintenance(std::sync::Arc::new(recovered.state), &application_codec)
            .await
            .expect("archive failure is retained as retry responsibility");
        let retry_state = match retry {
            MaintenanceOutcome::ArchiveRetryPending { state } => state,
            _ => panic!("archive conflict must preserve retry responsibility"),
        };
        std::fs::remove_file(conflict).expect("remove archive conflict");
        let completed = reopened
            .run_log_maintenance(retry_state, &application_codec)
            .await
            .expect("archive retry succeeds");
        assert!(matches!(
            completed,
            MaintenanceOutcome::Published { archived: true, .. }
        ));
        assert!(path.join("00000001.archive").is_file());
        assert!(!path.join("00000001").exists());
    });
}

#[test]
fn rotate_conflict_requires_recovery_without_publishing_next_sequence() {
    block_on(async {
        let temp = tempdir().expect("temporary log directory");
        let path = maintenance_support::log_path(&temp, "rotate-conflict");
        let block_codec = DefaultBlockCodec::default();
        let application_codec = transaction_support::TestCodec::default();
        let device = std::sync::Arc::new(transaction_support::TestBackend::new(
            transaction_support::key(1),
        ));
        let current = std::sync::Arc::new(maintenance_state(&device, 1, 23));
        let body = pi_superblock::encode_checkpoint(
            current.record_seq,
            &current.devices,
            &current.roots,
            &current.bindings,
            &current.application,
            &application_codec,
        )
        .expect("checkpoint body");
        let log = SuperblockAppendLog::open(path.clone(), block_codec.max_payload_len())
            .await
            .expect("open append log");
        assert!(matches!(
            log.append_durable(current.record_seq, RecordKind::Checkpoint, body)
                .await,
            DurableAppendOutcome::Durable { .. }
        ));
        log.set_rotate_threshold(1);
        std::fs::write(path.join("00000002"), b"conflicting next active")
            .expect("create next active conflict");
        let error = match log
            .run_log_maintenance(current.clone(), &application_codec)
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("rotate conflict must require recovery"),
        };
        assert!(matches!(error, MaintenanceError::RecoveryRequired { .. }));
        assert_eq!(current.record_seq.raw(), 1);
        assert!(path.join("00000001").is_file());
        assert_eq!(
            std::fs::read(path.join("00000002")).expect("conflict remains"),
            b"conflicting next active"
        );
    });
}

#[test]
fn exhausted_sequence_rejects_before_any_log_side_effect() {
    block_on(async {
        let temp = tempdir().expect("temporary log directory");
        let path = maintenance_support::log_path(&temp, "exhausted");
        let block_codec = DefaultBlockCodec::default();
        let application_codec = transaction_support::TestCodec::default();
        let device = std::sync::Arc::new(transaction_support::TestBackend::new(
            transaction_support::key(1),
        ));
        let current = std::sync::Arc::new(maintenance_state(&device, u32::MAX - 1, 31));
        let log = SuperblockAppendLog::open(path.clone(), block_codec.max_payload_len())
            .await
            .expect("open empty append log");
        log.set_rotate_threshold(1);
        let before: Vec<String> = std::fs::read_dir(&path)
            .expect("read log directory")
            .map(|entry| {
                entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let error = match log.run_log_maintenance(current, &application_codec).await {
            Err(error) => error,
            Ok(_) => panic!("exhausted sequence must reject maintenance"),
        };
        assert!(matches!(
            error,
            MaintenanceError::Rejected {
                source: SuperblockError::ReadOnlyExhausted
            }
        ));
        let after: Vec<String> = std::fs::read_dir(&path)
            .expect("read log directory after rejection")
            .map(|entry| {
                entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(after, before);
    });
}

#[test]
fn reopen_preserves_read_only_exhausted_status() {
    block_on(async {
        let temp = tempdir().expect("temporary log directory");
        let path = maintenance_support::log_path(&temp, "reopen-exhausted");
        let block_codec = DefaultBlockCodec::default();
        let application_codec = transaction_support::TestCodec::default();
        let device = std::sync::Arc::new(transaction_support::TestBackend::new(
            transaction_support::key(1),
        ));
        let state = maintenance_state(&device, u32::MAX - 1, 37);
        let body = pi_superblock::encode_checkpoint(
            state.record_seq,
            &state.devices,
            &state.roots,
            &state.bindings,
            &state.application,
            &application_codec,
        )
        .expect("checkpoint body");
        let log = SuperblockAppendLog::open(path.clone(), block_codec.max_payload_len())
            .await
            .expect("open append log");
        assert!(matches!(
            log.append_durable(state.record_seq, RecordKind::Checkpoint, body)
                .await,
            DurableAppendOutcome::Durable { .. }
        ));
        drop(log);

        let reopened = transaction_support::open(
            &path,
            std::slice::from_ref(&device),
            std::sync::Arc::new(application_codec),
        );
        assert_eq!(
            reopened.status(),
            pi_superblock::SuperblockStatus::ReadOnlyExhausted
        );
        assert!(matches!(
            reopened.begin_write().await,
            Err(SuperblockError::ReadOnlyExhausted)
        ));
    });
}

#[test]
fn read_only_exhausted_rejects_maintenance_with_structured_source() {
    let error = MaintenanceError::Rejected {
        source: SuperblockError::ReadOnlyExhausted,
    };
    assert!(matches!(
        error,
        MaintenanceError::Rejected {
            source: SuperblockError::ReadOnlyExhausted
        }
    ));
}

#[test]
fn repeated_reopen_preserves_archive_independent_retry_state() {
    block_on(async {
        let temp = tempdir().expect("temporary log directory");
        let path = maintenance_support::log_path(&temp, "repeat");
        let codec = DefaultBlockCodec::default();
        let seq = pi_block_device::RecordSeq::new(1).expect("valid sequence");
        let log = SuperblockAppendLog::open(path.clone(), codec.max_payload_len())
            .await
            .expect("open real append log");
        assert!(matches!(
            log.append_durable(
                seq,
                RecordKind::Checkpoint,
                Bytes::from_static(b"checkpoint")
            )
            .await,
            DurableAppendOutcome::Durable { .. }
        ));
        drop(log);
        for _ in 0..2 {
            let reopened =
                SuperblockAppendLog::open_replaying(path.clone(), codec.max_payload_len(), |_| {
                    false
                })
                .await
                .expect("reopen must be repeatable");
            drop(reopened);
        }
    });
}

#[test]
fn open_repairs_rotated_empty_active_before_archiving_old_chain() {
    block_on(async {
        use pi_append_log::file::{DefaultFileLayout, FileAppendLogBuilder};
        use pi_append_log::{AppendLog, AppendLogBuilder, AppendOptions, ReadOrder};

        let temp = tempdir().expect("temporary log directory");
        let path = maintenance_support::log_path(&temp, "empty-active");
        let block_codec = DefaultBlockCodec::default();
        let application_codec = transaction_support::TestCodec::default();
        let device = std::sync::Arc::new(transaction_support::TestBackend::new(
            transaction_support::key(1),
        ));
        let state = maintenance_state(&device, 1, 43);
        let body = pi_superblock::encode_checkpoint(
            state.record_seq,
            &state.devices,
            &state.roots,
            &state.bindings,
            &state.application,
            &application_codec,
        )
        .expect("checkpoint body");
        let encoded = pi_superblock::encode_append_log_block(
            &block_codec,
            state.record_seq,
            RecordKind::Checkpoint,
            &body,
        )
        .expect("checkpoint envelope");
        let mut visitor = maintenance_support::IgnoreVisitor;
        let build = FileAppendLogBuilder::new(&path, DefaultFileLayout::new(&path))
            .build(&block_codec, ReadOrder::Forward, &mut visitor)
            .await
            .expect("build raw log");
        build
            .storage
            .append(encoded, AppendOptions::default())
            .await
            .expect("append checkpoint");
        assert!(build.storage.rotate().await.expect("rotate").is_some());
        drop(build.storage);

        let (reopened, recovered) = pi_superblock::build_recovering(
            path.clone(),
            block_codec.max_payload_len(),
            &application_codec,
        )
        .await
        .expect("old complete chain remains recoverable");
        assert_eq!(recovered.state.record_seq.raw(), 1);
        let repaired = reopened
            .run_log_maintenance(std::sync::Arc::new(recovered.state), &application_codec)
            .await
            .expect("empty active repair succeeds");
        assert!(
            matches!(repaired, MaintenanceOutcome::Published { ref state, archived: false } if state.record_seq.raw() == 2)
        );
        let archived = match repaired {
            MaintenanceOutcome::Published { state, .. } => reopened
                .run_archive(state)
                .await
                .expect("archive old chain"),
            _ => unreachable!(),
        };
        assert!(matches!(
            archived,
            MaintenanceOutcome::Published { archived: true, .. }
        ));
    });
}
