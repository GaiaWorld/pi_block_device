use super::*;
use crate::SuperblockSnapshotApi;
use crate::{ApplicationCodecError, ApplicationEncoder, DeviceSlot, DeviceTable};
use bytes::Bytes;
use futures_lite::future::block_on;
use pi_block_device::{BlockDeviceState, LargeBlockCursor, RecordSeq};
use pi_file_block_device::{FileBlockDevice, FileBlockDeviceConfig};

#[derive(Clone)]
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
        out.put_slice(b"APP")
    }
    fn decode_state(&self, bytes: Bytes) -> Result<App, ApplicationCodecError> {
        if bytes.as_ref() != b"APP" {
            return Err(ApplicationCodecError::new("invalid APP"));
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
            return Err(ApplicationCodecError::new("invalid delta"));
        }
        Ok(())
    }
}
struct Resolver(Arc<FileBlockDevice>);
impl DeviceResolver<FileBlockDevice> for Resolver {
    async fn resolve(
        &self,
        _: DeviceKey,
        _: &DeviceRegistration,
    ) -> Result<Arc<FileBlockDevice>, ResolveError> {
        Ok(Arc::clone(&self.0))
    }
}
fn config(path: PathBuf) -> SuperblockLogConfig {
    SuperblockLogConfig {
        path,
        rotate_threshold: 1_000_000,
        append_log_options: pi_append_log::AppendOptions { durable: true },
    }
}
async fn device(temp: &tempfile::TempDir) -> Arc<FileBlockDevice> {
    // SAFETY: exclusive temporary root; all owners are scoped to the test.
    unsafe {
        FileBlockDevice::create(FileBlockDeviceConfig {
            root: temp.path().join("device"),
        })
    }
    .await
    .unwrap()
}
async fn open_cursor(raw: u64) {
    let temp = tempfile::tempdir().unwrap();
    let device = device(&temp).await;
    let mut state = BlockDeviceState::initial().unwrap();
    state.large_blocks.next_id = LargeBlockCursor::new(raw).unwrap();
    let table = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: device.device_id(),
            label: DeviceLabel::new("main".into()).unwrap(),
        },
        state,
    )])
    .unwrap();
    let seq = RecordSeq::new(1).unwrap();
    let body = crate::encode_checkpoint(
        seq,
        &table,
        &rpds::RedBlackTreeMapSync::new_sync(),
        &rpds::RedBlackTreeMapSync::new_sync(),
        &App,
        &Codec,
    )
    .unwrap();
    let path = temp.path().join("log");
    let log = SuperblockAppendLog::open(path.clone(), u32::MAX as usize)
        .await
        .unwrap();
    assert!(matches!(
        log.append_durable(seq, crate::RecordKind::Checkpoint, body)
            .await,
        crate::DurableAppendOutcome::Durable { .. }
    ));
    drop(log);
    let sb = Superblock::open(
        config(path),
        Arc::new(Resolver(Arc::clone(&device))),
        Arc::new(Codec),
    )
    .await
    .unwrap();
    assert_eq!(sb.status(), SuperblockStatus::Ready);
    assert_eq!(
        sb.open_snapshot()
            .unwrap()
            .device_state(DeviceKey::new(1).unwrap())
            .unwrap()
            .large_blocks
            .next_id
            .raw(),
        raw
    );
    // 介质从未存在数据对象，恢复不得按历史游标伪造退休队列项。
    assert_eq!(device.reclaim_retirements().await.unwrap().attempted, 0);
}
#[test]
fn small_cursor_open_does_not_enqueue_nonexistent_history() {
    block_on(open_cursor(4));
}
#[test]
fn exhausted_cursor_open_does_not_enumerate_history() {
    block_on(open_cursor((1_u64 << 60) - 1));
}

#[test]
fn publication_poison_preserves_old_objects_after_real_durable_release() {
    block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let device = device(&temp).await;
        let path = temp.path().join("log");
        let sb = Superblock::create(
            config(path.clone()),
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
        let mut tx = sb.begin_write().await.unwrap();
        let payload = Bytes::from(vec![7; 524289]);
        let block = tx
            .write_on(DeviceKey::new(1).unwrap(), payload.clone())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(tx);
        let old = sb.published.load_full();
        let mut tx = sb.begin_write().await.unwrap();
        tx.release(vec![block]).unwrap();
        // 注入真实锁poison，metadata仍走真实append与durable路径。
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _lock = sb.publication_access.lock().unwrap();
                panic!("publication poison");
            }))
            .is_err()
        );
        let result = tx.commit().await.unwrap();
        assert_eq!(result.record_seq.raw(), 3);
        assert_eq!(result.status, SuperblockStatus::RecoveryRequired);
        drop(tx);
        assert!(Arc::ptr_eq(&old, &sb.published.load_full()));
        let report = device.reclaim_retirements().await.unwrap();
        assert_eq!(
            report.attempted, 0,
            "unpublished retirements must not be admitted"
        );
        let lease = device
            .acquire_snapshot_lease(Arc::new(
                old.devices.get(block.device).unwrap().state().clone(),
            ))
            .unwrap();
        assert_eq!(
            device.read(&lease, block.block).await.unwrap().as_ref(),
            payload.as_ref()
        );
        drop(lease);
        drop(old);
        drop(sb);
        let (log, recovered) = crate::build_recovering(path.clone(), u32::MAX as usize, &Codec)
            .await
            .unwrap();
        assert_eq!(
            recovered.state.record_seq.raw(),
            3,
            "release metadata really is durable"
        );
        assert!(
            recovered
                .state
                .devices
                .get(block.device)
                .unwrap()
                .state()
                .large_blocks
                .entries
                .is_empty()
        );
        drop(log);
        let reopened = Superblock::open(
            config(path),
            Arc::new(Resolver(Arc::clone(&device))),
            Arc::new(Codec),
        )
        .await
        .unwrap();
        assert_eq!(reopened.status(), SuperblockStatus::Ready);
        assert!(
            device
                .acquire_snapshot_lease(Arc::new(
                    reopened
                        .published
                        .load_full()
                        .devices
                        .get(block.device)
                        .unwrap()
                        .state()
                        .clone()
                ))
                .is_ok()
        );
        assert!(
            device.inspect().await.unwrap().empty,
            "reconcile removes the real orphan"
        );
    });
}
