use bytes::Bytes;
use futures_lite::future::block_on;
use pi_block_device::{BlockDeviceBackend, DeviceKey};
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig};
use pi_superblock::*;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
struct App;
impl ApplicationState for App {
    type Delta = ();
}
struct Codec;
impl ApplicationCodec<App> for Codec {
    fn encode_state(
        &self,
        _: &App,
        out: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        out.put_slice(b"APP1")
    }
    fn decode_state(&self, bytes: Bytes) -> Result<App, ApplicationCodecError> {
        if bytes.as_ref() != b"APP1" {
            return Err(ApplicationCodecError::new("invalid complete APP1 payload"));
        }
        Ok(App)
    }
    fn encode_delta(
        &self,
        _: &(),
        _: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
    }
    fn decode_delta(&self, bytes: Bytes) -> Result<(), ApplicationCodecError> {
        if !bytes.is_empty() {
            return Err(ApplicationCodecError::new("nonempty delta"));
        }
        Ok(())
    }
}
struct Resolver(Arc<FileBlockDevice>);
impl DeviceResolver<FileBlockDevice> for Resolver {
    async fn resolve(
        &self,
        key: DeviceKey,
        _: &DeviceRegistration,
    ) -> Result<Arc<FileBlockDevice>, ResolveError> {
        assert_eq!(key.raw(), 1);
        Ok(Arc::clone(&self.0))
    }
}
fn config(path: &std::path::Path) -> SuperblockLogConfig {
    SuperblockLogConfig {
        path: path.to_owned(),
        rotate_threshold: 1_000_000,
        append_log_options: pi_append_log::AppendOptions { durable: false },
    }
}
type Owner = Superblock<App, Codec, FileBlockDevice, Resolver>;

#[test]
fn real_inspect_excludes_only_identity_manifest_and_preserves_unknown_object() {
    block_on(async {
        let (temp, _sb, device) = fixture().await;
        assert!(device.inspect().await.unwrap().empty);
        let unknown = temp.path().join("device").join("unknown.bin");
        std::fs::write(&unknown, b"foreign evidence").unwrap();
        let report = device.inspect().await.unwrap();
        assert!(!report.empty);
        assert_eq!(report.observed_objects, 1);
        assert_eq!(std::fs::read(unknown).unwrap(), b"foreign evidence");
    });
}

#[test]
fn real_batch_allocations_are_written_before_one_delta_and_reopen() {
    block_on(async {
        let (temp, sb, device) = fixture().await;
        let key = DeviceKey::new(1).unwrap();
        let mut tx = sb.begin_write().await.unwrap();
        let first = tx.alloc_on(key, 4096).unwrap();
        let second = tx.alloc_on(key, 524289).unwrap();
        tx.write_batch(vec![
            BlockWrite::new(first, Bytes::from(vec![3; 4096])).unwrap(),
            BlockWrite::new(second, Bytes::from(vec![4; 524289])).unwrap(),
        ])
        .await
        .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        drop(sb);
        let (log, recovered) = build_recovering(temp.path().join("log"), u32::MAX as usize, &Codec)
            .await
            .unwrap();
        assert_eq!(recovered.state.record_seq.raw(), 2);
        let state = recovered.state.devices.get(key).unwrap();
        assert_eq!(
            state.state().levels[0]
                .versions
                .get(0)
                .unwrap()
                .as_ref()
                .unwrap()
                .released_at
                .len(),
            1
        );
        assert_eq!(state.state().large_blocks.entries.size(), 1);
        drop(log);
        let mut kinds = Vec::new();
        drop(
            SuperblockAppendLog::open_replaying(
                temp.path().join("log"),
                u32::MAX as usize,
                |record| {
                    kinds.push(record.header.record_kind);
                    false
                },
            )
            .await
            .unwrap(),
        );
        assert_eq!(kinds, vec![RecordKind::Checkpoint, RecordKind::Delta]);
        let sb = Superblock::open(
            config(&temp.path().join("log")),
            Arc::new(Resolver(device)),
            Arc::new(Codec),
        )
        .await
        .unwrap();
        assert_eq!(sb.open_snapshot().unwrap().record_seq().raw(), 2);
    });
}

#[test]
fn actual_writer_gate_wait_cancellation_has_no_backend_or_log_side_effect() {
    block_on(async {
        let (_temp, sb, device) = fixture().await;
        let first = sb.begin_write().await.unwrap();
        let mut waiting = Box::pin(sb.begin_write());
        assert!(
            futures_lite::future::poll_once(waiting.as_mut())
                .await
                .is_none()
        );
        drop(waiting);
        assert!(device.inspect().await.unwrap().empty);
        assert_eq!(sb.open_snapshot().unwrap().record_seq().raw(), 1);
        drop(first);
        drop(sb.begin_write().await.unwrap());
    });
}

#[test]
fn reopened_last_sequence_is_readonly_and_rejects_writes_without_wait() {
    block_on(async {
        let (temp, sb, device) = fixture().await;
        let snapshot = sb.open_snapshot().unwrap();
        let table = DeviceTable::new(vec![DeviceSlot::new(
            snapshot.devices().next().unwrap().1.clone(),
            snapshot
                .device_state(DeviceKey::new(1).unwrap())
                .unwrap()
                .clone(),
        )])
        .unwrap();
        drop(snapshot);
        drop(sb);
        let seq = pi_block_device::RecordSeq::new(u32::MAX - 1).unwrap();
        let body = encode_checkpoint(
            seq,
            &table,
            &rpds::RedBlackTreeMapSync::new_sync(),
            &rpds::RedBlackTreeMapSync::new_sync(),
            &App,
            &Codec,
        )
        .unwrap();
        let path = temp.path().join("last-log");
        let log = SuperblockAppendLog::open(path.clone(), u32::MAX as usize)
            .await
            .unwrap();
        assert!(matches!(
            log.append_durable(seq, RecordKind::Checkpoint, body).await,
            DurableAppendOutcome::Durable { .. }
        ));
        drop(log);
        let sb = Superblock::open(config(&path), Arc::new(Resolver(device)), Arc::new(Codec))
            .await
            .unwrap();
        assert_eq!(sb.status(), SuperblockStatus::ReadOnlyExhausted);
        assert!(matches!(
            sb.begin_write().await,
            Err(SuperblockError::ReadOnlyExhausted)
        ));
        assert_eq!(sb.open_snapshot().unwrap().record_seq(), seq);
    });
}
async fn fixture() -> (tempfile::TempDir, Owner, Arc<FileBlockDevice>) {
    let temp = tempfile::tempdir().unwrap();
    // SAFETY: isolated root owned exclusively by this fixture.
    let device = unsafe {
        FileBlockDevice::create(FileBlockDeviceConfig {
            root: temp.path().join("device"),
        })
    }
    .await
    .unwrap();
    let sb = Superblock::create(
        config(&temp.path().join("log")),
        vec![InitialDevice {
            label: DeviceLabel::new("main".into()).unwrap(),
            device: Arc::clone(&device),
        }],
        Arc::new(Resolver(Arc::clone(&device))),
        Arc::new(Codec),
        App,
    )
    .await
    .unwrap();
    (temp, sb, device)
}

#[test]
fn unwritten_commit_closes_and_releases_writer_without_append() {
    block_on(async {
        let (_temp, sb, _device) = fixture().await;
        let mut tx = sb.begin_write().await.unwrap();
        tx.alloc_on(DeviceKey::new(1).unwrap(), 4096).unwrap();
        assert!(matches!(
            tx.commit().await,
            Err(SuperblockError::CommitFailed)
        ));
        assert!(matches!(
            tx.commit().await,
            Err(SuperblockError::TransactionClosed)
        ));
        assert_eq!(sb.open_snapshot().unwrap().record_seq().raw(), 1);
        let mut next = sb.begin_write().await.unwrap();
        assert_eq!(next.commit().await.unwrap().record_seq.raw(), 2);
    });
}

#[test]
fn stream_failures_preserve_error_and_close_without_publishing() {
    block_on(async {
        for mode in 0..3 {
            let (_temp, sb, _device) = fixture().await;
            let mut tx = sb.begin_write().await.unwrap();
            let items = match mode {
                0 => vec![Ok(Bytes::from_static(b"a"))],
                1 => vec![Ok(Bytes::from_static(b"abc"))],
                _ => vec![Err("item evidence")],
            };
            let error = tx
                .write_stream_on(
                    DeviceKey::new(1).unwrap(),
                    2,
                    futures_lite::stream::iter(items),
                )
                .await
                .unwrap_err();
            match mode {
                0 => assert!(matches!(
                    error,
                    StreamWriteError::StreamTooShort {
                        written: 1,
                        logical_len: 2
                    }
                )),
                1 => assert!(matches!(
                    error,
                    StreamWriteError::StreamTooLong {
                        written: 0,
                        logical_len: 2
                    }
                )),
                _ => assert!(matches!(error, StreamWriteError::Item("item evidence"))),
            }
            assert!(matches!(
                tx.commit().await,
                Err(SuperblockError::TransactionClosed)
            ));
            assert_eq!(sb.open_snapshot().unwrap().record_seq().raw(), 1);
            drop(sb.begin_write().await.unwrap());
        }
    });
}

#[test]
fn cancelling_actual_pending_stream_releases_writer_and_does_not_append() {
    block_on(async {
        let (_temp, sb, _device) = fixture().await;
        let mut tx = sb.begin_write().await.unwrap();
        let mut write = Box::pin(tx.write_stream_on(
            DeviceKey::new(1).unwrap(),
            4096,
            futures_lite::stream::pending::<Result<Bytes, &'static str>>(),
        ));
        assert!(
            futures_lite::future::poll_once(write.as_mut())
                .await
                .is_none()
        );
        drop(write);
        assert!(matches!(
            tx.commit().await,
            Err(SuperblockError::TransactionClosed)
        ));
        assert_eq!(sb.status(), SuperblockStatus::Ready);
        assert_eq!(sb.open_snapshot().unwrap().record_seq().raw(), 1);
        drop(sb.begin_write().await.unwrap());
    });
}

#[test]
fn zero_stream_length_is_rejected_before_allocation_and_keeps_transaction_open() {
    block_on(async {
        let (_temp, sb, _device) = fixture().await;
        let mut tx = sb.begin_write().await.unwrap();
        assert!(matches!(
            tx.write_stream_on(
                DeviceKey::new(1).unwrap(),
                0,
                futures_lite::stream::empty::<Result<Bytes, &'static str>>()
            )
            .await,
            Err(StreamWriteError::Superblock(
                SuperblockError::InvalidBlockRef
            ))
        ));
        assert_eq!(tx.commit().await.unwrap().record_seq.raw(), 2);
    });
}

#[test]
fn old_snapshot_and_read_buffer_protect_retired_large_block() {
    block_on(async {
        let (_temp, sb, device) = fixture().await;
        let mut tx = sb.begin_write().await.unwrap();
        let payload = Bytes::from(vec![9; 524289]);
        let block = tx
            .write_on(DeviceKey::new(1).unwrap(), payload.clone())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let old = sb.open_snapshot().unwrap();
        let buffer = sb.read(block).await.unwrap();
        let mut tx = sb.begin_write().await.unwrap();
        tx.release(vec![block]).unwrap();
        assert!(matches!(
            tx.read(block).await,
            Err(SuperblockError::InvalidBlockRef)
        ));
        tx.commit().await.unwrap();
        drop(tx);
        assert!(matches!(
            sb.read(block).await,
            Err(SuperblockError::InvalidBlockRef)
        ));
        assert_eq!(old.read(block).await.unwrap().as_ref(), payload.as_ref());
        assert_eq!(device.reclaim_retirements().await.unwrap().guarded, 1);
        drop(old);
        assert_eq!(device.reclaim_retirements().await.unwrap().guarded, 1);
        assert_eq!(buffer.as_ref(), payload.as_ref());
        drop(buffer);
        assert_eq!(device.reclaim_retirements().await.unwrap().completed, 1);
    });
}

#[test]
fn real_initial_checkpoint_snapshot_and_reopen() {
    block_on(async {
        let temp = tempfile::tempdir().unwrap();
        // SAFETY: isolated device root, no external mutations or overlapping owners.
        let device = unsafe {
            FileBlockDevice::create(FileBlockDeviceConfig {
                root: temp.path().join("device"),
            })
        }
        .await
        .unwrap();
        let id = device.device_id();
        let resolver = Arc::new(Resolver(Arc::clone(&device)));
        let codec = Arc::new(Codec);
        let path = temp.path().join("log");
        let sb = Superblock::create(
            config(&path),
            vec![InitialDevice {
                label: DeviceLabel::new("main".into()).unwrap(),
                device,
            }],
            Arc::clone(&resolver),
            Arc::clone(&codec),
            App,
        )
        .await
        .unwrap();
        let snapshot = sb.open_snapshot().unwrap();
        assert_eq!(snapshot.record_seq().raw(), 1);
        assert_eq!(snapshot.devices().next().unwrap().1.id, id);
        assert_eq!(
            snapshot
                .device_state(DeviceKey::new(1).unwrap())
                .unwrap()
                .levels
                .len(),
            15
        );
        assert_eq!(snapshot.roots().count(), 0);
        assert_eq!(snapshot.bindings().count(), 0);
        let mut tx = sb.begin_write().await.unwrap();
        let payload = Bytes::from(vec![7; 4096]);
        let block = tx
            .write_on(DeviceKey::new(1).unwrap(), payload.clone())
            .await
            .unwrap();
        assert!(matches!(
            tx.read(block).await,
            Err(SuperblockError::InvalidBlockRef)
        ));
        tx.add_root(block).unwrap();
        let name = BindingName::new("payload".into()).unwrap();
        tx.bind(name.clone(), block).unwrap();
        assert_eq!(tx.get_binding(&name), Some(block));
        assert_eq!(tx.commit().await.unwrap().record_seq.raw(), 2);
        assert!(matches!(
            tx.alloc_on(DeviceKey::new(1).unwrap(), 1),
            Err(SuperblockError::TransactionClosed)
        ));
        drop(tx);
        assert_eq!(sb.read(block).await.unwrap().as_ref(), payload.as_ref());
        assert_eq!(snapshot.record_seq().raw(), 1);
        drop(snapshot);
        drop(sb);
        drop(resolver);
        // SAFETY: the prior owner and all mmap buffers have been dropped; same isolated root.
        let reopened = unsafe {
            FileBlockDevice::open(
                FileBlockDeviceConfig {
                    root: temp.path().join("device"),
                },
                Some(id),
            )
        }
        .await
        .unwrap();
        assert_eq!(reopened.device_id(), id);
        let sb = Superblock::open(config(&path), Arc::new(Resolver(reopened)), codec)
            .await
            .unwrap();
        assert_eq!(sb.status(), SuperblockStatus::Ready);
        let snapshot = sb.open_snapshot().unwrap();
        assert_eq!(snapshot.record_seq().raw(), 2);
        assert_eq!(snapshot.get_binding(&name), Some(block));
        assert_eq!(snapshot.get_root(block).unwrap().published_at.raw(), 2);
        assert_eq!(
            snapshot.read(block).await.unwrap().as_ref(),
            payload.as_ref()
        );
        let mut tx = sb.begin_write().await.unwrap();
        assert_eq!(tx.read(block).await.unwrap().as_ref(), payload.as_ref());
        let stream_block = tx
            .write_stream_on(
                DeviceKey::new(1).unwrap(),
                4096,
                futures_lite::stream::iter(vec![
                    Ok::<_, &'static str>(Bytes::new()),
                    Ok(Bytes::from(vec![8; 1])),
                    Ok(Bytes::from(vec![8; 4095])),
                ]),
            )
            .await
            .unwrap();
        assert_eq!(tx.commit().await.unwrap().record_seq.raw(), 3);
        drop(tx);
        assert_eq!(sb.read(stream_block).await.unwrap().as_ref(), &[8; 4096]);
    });
}
