//! Public transaction acceptance against real file devices and durable metadata.
#[path = "dynamic_commit/support.rs"]
mod support;
use bytes::Bytes;
use futures_lite::future::block_on;
use pi_block_device::{BlockDeviceBackend, DeviceOperation};
use pi_superblock::*;
use std::sync::Arc;
use support::*;

#[test]
fn blocked_prepares_precede_one_durable_delta_publication_and_retirement() {
    block_on(async {
        let (temp, sb, devices) = fixture().await;
        let path = temp.path().join("log");
        let mut seed = sb.begin_write().await.unwrap();
        let old_block = seed
            .write_on(key(1), Bytes::from(vec![9; 524289]))
            .await
            .unwrap();
        devices[0].gate.release(true);
        assert_eq!(seed.commit().await.unwrap().record_seq.raw(), 2);
        drop(seed);
        let old = sb.open_snapshot().unwrap();
        let baseline = records(&path).await;
        assert_eq!(
            baseline,
            vec![(1, RecordKind::Checkpoint), (2, RecordKind::Delta)]
        );
        devices[0].events.lock().unwrap().clear();
        let weak = Arc::downgrade(&sb);
        *devices[0].observer.lock().unwrap() = Some(Box::new(move |retired| {
            assert_eq!(retired.len(), 1);
            let owner = weak.upgrade().unwrap();
            let published = owner.open_snapshot().unwrap();
            assert_eq!(
                published.record_seq().raw(),
                3,
                "retirement sees new publication"
            );
            assert!(
                published
                    .device_state(key(1))
                    .unwrap()
                    .large_blocks
                    .entries
                    .get(&old_block.block.large_id().unwrap())
                    .is_none()
            );
        }));
        let mut tx = sb.begin_write().await.unwrap();
        tx.release(vec![old_block]).unwrap();
        let mut written = Vec::new();
        // Reverse write admission must not determine consuming prepare order.
        for raw in [3, 2, 1] {
            let payload = Bytes::from(vec![raw as u8; 4096]);
            let block = tx.write_on(key(raw), payload.clone()).await.unwrap();
            let name = BindingName::new(format!("payload-{raw}").into()).unwrap();
            tx.add_root(block).unwrap();
            tx.bind(name.clone(), block).unwrap();
            written.push((block, name, payload));
        }
        assert_eq!(
            *devices[0].events.lock().unwrap(),
            vec![(3, "begin"), (2, "begin"), (1, "begin")]
        );
        let mut commit = Box::pin(tx.commit());
        for raw in 1..=3 {
            reach_gate(commit.as_mut(), &devices[0].events, raw).await;
            let events = devices[0].events.lock().unwrap().clone();
            let prepares: Vec<_> = events
                .iter()
                .filter(|(_, event)| *event == "prepare")
                .map(|(key, _)| *key)
                .collect();
            assert_eq!(prepares, (1..=raw).collect::<Vec<_>>());
            assert!(!events.iter().any(|(_, event)| *event == "retire"));
            assert_eq!(
                records(&path).await,
                baseline,
                "no metadata before all prepares return"
            );
            let current = sb.open_snapshot().unwrap();
            assert_eq!(current.record_seq().raw(), 2);
            for (block, name, _) in &written {
                assert!(current.get_root(*block).is_none());
                assert_eq!(current.get_binding(name), None);
            }
            assert_eq!(old.read(old_block).await.unwrap().as_ref(), &[9; 524289]);
            devices[raw as usize - 1].gate.release(true);
        }
        assert_eq!(commit.await.unwrap().record_seq.raw(), 3);
        drop(tx);
        let events = devices[0].events.lock().unwrap().clone();
        assert_eq!(
            &events[3..],
            &[
                (1, "prepare"),
                (1, "prepared"),
                (2, "prepare"),
                (2, "prepared"),
                (3, "prepare"),
                (3, "prepared"),
                (1, "retire")
            ]
        );
        assert_eq!(
            records(&path).await,
            vec![
                (1, RecordKind::Checkpoint),
                (2, RecordKind::Delta),
                (3, RecordKind::Delta)
            ]
        );
        let published = sb.open_snapshot().unwrap();
        for (block, name, payload) in &written {
            assert_eq!(published.get_binding(name), Some(*block));
            assert_eq!(published.get_root(*block).unwrap().published_at.raw(), 3);
            assert_eq!(
                published.read(*block).await.unwrap().as_ref(),
                payload.as_ref()
            );
        }
        assert_eq!(published.record_seq().raw(), 3);
        assert_eq!(old.record_seq().raw(), 2);
        assert_eq!(devices[0].reclaim_retirements().await.unwrap().guarded, 1);
        drop(old);
        drop(published);
        drop(sb);
        let recovered = Owner::open(
            config(&path),
            Arc::new(Resolver(devices.clone())),
            Arc::new(Codec),
        )
        .await
        .unwrap();
        assert_eq!(recovered.open_snapshot().unwrap().record_seq().raw(), 3);
        for (block, name, payload) in written {
            let snapshot = recovered.open_snapshot().unwrap();
            assert_eq!(snapshot.get_binding(&name), Some(block));
            assert_eq!(snapshot.get_root(block).unwrap().published_at.raw(), 3);
            assert_eq!(
                snapshot.read(block).await.unwrap().as_ref(),
                payload.as_ref()
            );
        }
    });
}

#[test]
fn partial_prepare_failure_stops_later_devices_preserves_residue_and_recovers() {
    block_on(async {
        let (temp, sb, devices) = fixture().await;
        let path = temp.path().join("log");
        let baseline = records(&path).await;
        let mut tx = sb.begin_write().await.unwrap();
        for raw in [3, 2, 1] {
            tx.write_on(key(raw), Bytes::from(vec![raw as u8; 4096]))
                .await
                .unwrap();
        }
        let before: Vec<_> = (1..=3)
            .map(|raw| image(&temp.path().join(format!("device-{raw}"))))
            .collect();
        let mut commit = Box::pin(tx.commit());
        reach_gate(commit.as_mut(), &devices[0].events, 1).await;
        assert_eq!(records(&path).await, baseline);
        devices[0].gate.release(true);
        reach_gate(commit.as_mut(), &devices[0].events, 2).await;
        assert_eq!(records(&path).await, baseline);
        assert_eq!(sb.open_snapshot().unwrap().record_seq().raw(), 1);
        devices[1].gate.release(false);
        let error = commit.await.unwrap_err();
        match error {
            SuperblockError::Device { source } => {
                assert_eq!(source.device_key, Some(key(2)));
                assert_eq!(source.operation, DeviceOperation::Prepare);
                assert!(
                    source
                        .source
                        .to_string()
                        .contains("controlled prepare failure")
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(matches!(
            tx.commit().await,
            Err(SuperblockError::TransactionClosed)
        ));
        drop(tx);
        assert_eq!(
            &devices[0].events.lock().unwrap()[3..],
            &[(1, "prepare"), (1, "prepared"), (2, "prepare")]
        );
        assert_eq!(records(&path).await, baseline);
        for raw in 1..=3 {
            assert_eq!(
                image(&temp.path().join(format!("device-{raw}"))),
                before[raw - 1],
                "failure/Drop must not unlink prepared or unprepared residue"
            );
        }
        let mut next = sb.begin_write().await.unwrap();
        assert_eq!(next.commit().await.unwrap().record_seq.raw(), 2);
        drop(next);
        drop(sb);
        // Release every backend owner before unsafe same-ID reopen; no mmap/leases survive.
        let ids: Vec<_> = devices.iter().map(|device| device.device_id()).collect();
        drop(devices);
        let mut fresh = Vec::new();
        for (index, id) in ids.into_iter().enumerate() {
            // SAFETY: old owners dropped above; original TempDir still exclusively owns each stable root.
            let file = unsafe {
                pi_file_block_device::FileBlockDevice::open(
                    pi_file_block_device::FileBlockDeviceConfig {
                        root: temp.path().join(format!("device-{}", index + 1)),
                    },
                    Some(id),
                )
            }
            .await
            .unwrap();
            fresh.push(Backend::wrap(
                file,
                key(index as u32 + 1),
                Arc::new(std::sync::Mutex::new(Vec::new())),
            ));
        }
        let recovered = Owner::open(
            config(&path),
            Arc::new(Resolver(fresh.clone())),
            Arc::new(Codec),
        )
        .await
        .unwrap();
        assert_eq!(recovered.open_snapshot().unwrap().record_seq().raw(), 2);
        for device in &fresh {
            assert!(device.inspect().await.unwrap().empty);
        }
        let mut next = recovered.begin_write().await.unwrap();
        let block = next
            .write_on(key(1), Bytes::from(vec![7; 4096]))
            .await
            .unwrap();
        fresh[0].gate.release(true);
        assert_eq!(next.commit().await.unwrap().record_seq.raw(), 3);
        drop(next);
        assert_eq!(recovered.read(block).await.unwrap().as_ref(), &[7; 4096]);
        drop(recovered);
        drop(fresh);
    });
}
