mod snapshot_support;
use futures_lite::future::block_on;
use pi_block_device::DeviceKey;
use pi_superblock::{SuperblockError, SuperblockSnapshotApi};
use snapshot_support::{Backend, Delta, create};
use std::sync::Arc;

#[test]
fn snapshot_is_fixed_across_commit_and_queries_application() {
    block_on(async {
        let temp = tempfile::tempdir().expect("temp");
        let backend = Arc::new(Backend::new(1, false));
        let store = create(temp.path(), &[backend]).await;
        let old = store.open_snapshot().expect("snapshot");
        let mut tx = store.begin_write().await.expect("tx");
        tx.apply_application(Delta(2)).expect("delta");
        tx.commit().await.expect("commit");
        let new = store.open_snapshot().expect("snapshot");
        assert_eq!(old.application().0, 1);
        assert_eq!(new.application().0, 2);
        assert!(old.record_seq() < new.record_seq());
    });
}

#[test]
fn lease_failure_is_all_or_nothing_and_stops_in_key_order() {
    block_on(async {
        let temp = tempfile::tempdir().expect("temp");
        let one = Arc::new(Backend::new(1, false));
        let two = Arc::new(Backend::new(2, true));
        let three = Arc::new(Backend::new(3, false));
        let store = create(temp.path(), &[one.clone(), two.clone(), three.clone()]).await;
        let error = store.open_snapshot().err().expect("failure");
        match error {
            SuperblockError::Device { source } => {
                assert_eq!(source.device_key, Some(DeviceKey::new(2).expect("key")));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(one.calls.lock().expect("calls").drops, 1);
        assert!(three.calls.lock().expect("calls").events.is_empty());
    });
}
