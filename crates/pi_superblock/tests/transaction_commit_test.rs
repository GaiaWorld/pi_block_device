mod transaction_support;

use std::pin::pin;
use std::sync::Arc;

use bytes::Bytes;
use pi_block_device::{BlockId, BlockRef, DeviceOperation};
use pi_superblock::{BindingName, SuperblockError, SuperblockStatus};
use tempfile::tempdir;

use transaction_support::{Failure, TestBackend, TestCodec, TestDelta, create, key, open};

#[test]
fn begin_write_is_lazy_and_reuses_one_batch_per_participant() {
    let temp = tempdir().expect("tempdir");
    let devices = [
        Arc::new(TestBackend::new(key(1))),
        Arc::new(TestBackend::new(key(2))),
        Arc::new(TestBackend::new(key(3))),
    ];
    let superblock = create(temp.path(), &devices, Arc::new(TestCodec::default()));
    let mut transaction = futures_lite::future::block_on(superblock.begin_write()).expect("begin");
    for device in &devices {
        assert!(device.calls.lock().expect("calls").begins == 0);
    }
    let _logical = transaction.alloc_on(key(2), 2).expect("logical allocation");
    for device in &devices {
        assert!(device.calls.lock().expect("calls").begins == 0);
    }

    futures_lite::future::block_on(transaction.write_on(key(3), Bytes::from_static(b"three")))
        .expect("write device 3");
    futures_lite::future::block_on(transaction.write_on(key(1), Bytes::from_static(b"one")))
        .expect("write device 1");
    futures_lite::future::block_on(transaction.write_on(key(3), Bytes::from_static(b"again")))
        .expect("second write device 3");

    let first = devices[0].calls.lock().expect("calls").begins;
    let second = devices[1].calls.lock().expect("calls").begins;
    let third = devices[2].calls.lock().expect("calls").begins;
    // 批次不再携带事务身份，只验证懒创建与同设备复用。
    assert_eq!(first, 1);
    assert_eq!(second, 0);
    assert_eq!(third, 1);
}

#[test]
fn logical_only_commit_never_creates_backend_session() {
    let temp = tempdir().expect("tempdir");
    let device = Arc::new(TestBackend::new(key(1)));
    let codec = Arc::new(TestCodec::default());
    let superblock = create(temp.path(), std::slice::from_ref(&device), codec);
    let mut transaction = futures_lite::future::block_on(superblock.begin_write()).expect("begin");
    transaction
        .apply_application(TestDelta(7))
        .expect("application delta");
    let result = futures_lite::future::block_on(transaction.commit()).expect("commit");
    assert_eq!(result.record_seq().raw(), 2);
    assert_eq!(result.status, SuperblockStatus::Ready);
    assert!(device.calls.lock().expect("calls").begins == 0);
}

#[test]
fn single_writer_waits_drop_releases_and_cancelled_waiter_does_not_hold_gate() {
    let temp = tempdir().expect("tempdir");
    let device = Arc::new(TestBackend::new(key(1)));
    let superblock = create(
        temp.path(),
        std::slice::from_ref(&device),
        Arc::new(TestCodec::default()),
    );
    let first = futures_lite::future::block_on(superblock.begin_write()).expect("first");
    {
        let mut waiting = pin!(superblock.begin_write());
        assert!(
            futures_lite::future::block_on(futures_lite::future::poll_once(&mut waiting)).is_none()
        );
    }

    let mut next = pin!(superblock.begin_write());
    assert!(futures_lite::future::block_on(futures_lite::future::poll_once(&mut next)).is_none());
    drop(first);
    let second = futures_lite::future::block_on(next).expect("second after release");
    drop(second);
    futures_lite::future::block_on(superblock.begin_write()).expect("gate remains usable");
}

#[test]
fn consecutive_commits_publish_latest_state_and_open_recovers_final_state() {
    let temp = tempdir().expect("tempdir");
    let device = Arc::new(TestBackend::new(key(1)));
    let codec = Arc::new(TestCodec::default());
    let superblock = create(temp.path(), std::slice::from_ref(&device), codec.clone());

    let mut first = futures_lite::future::block_on(superblock.begin_write()).expect("first");
    first.apply_application(TestDelta(2)).expect("delta 2");
    assert_eq!(
        futures_lite::future::block_on(first.commit())
            .expect("seq2")
            .record_seq
            .raw(),
        2
    );

    let mut second = futures_lite::future::block_on(superblock.begin_write()).expect("second");
    assert_eq!(second.application().value, 2);
    let block = second.alloc_on(key(1), 4).expect("allocate");
    // 提交前必须完整写入每笔分配，不能只保留逻辑游标变化。
    let write = pi_superblock::BlockWrite::new(block, Bytes::from_static(b"data"))
        .expect("exact allocation length");
    futures_lite::future::block_on(second.write_batch(vec![write])).expect("write allocation");
    let block_ref =
        futures_lite::future::block_on(second.write_on(key(1), Bytes::from_static(b"root")))
            .expect("write root");
    second.add_root(block_ref).expect("root");
    second
        .bind(BindingName::new(b"main".to_vec()).expect("name"), block_ref)
        .expect("binding");
    second.apply_application(TestDelta(3)).expect("delta 3");
    assert_eq!(second.application().roots, 1);
    assert_eq!(second.application().bindings, 1);
    assert_eq!(
        futures_lite::future::block_on(second.commit())
            .expect("seq3")
            .record_seq
            .raw(),
        3
    );
    drop(first);
    drop(second);
    drop(superblock);

    let reopened = open(temp.path(), std::slice::from_ref(&device), codec);
    let recovered =
        futures_lite::future::block_on(reopened.begin_write()).expect("recovered transaction");
    assert_eq!(recovered.application().value, 3);
    assert_eq!(recovered.application().roots, 1);
    assert_eq!(recovered.application().bindings, 1);
}

#[test]
fn commit_prepares_each_existing_participant_once() {
    let temp = tempdir().expect("tempdir");
    let devices = [
        Arc::new(TestBackend::new(key(1))),
        Arc::new(TestBackend::new(key(2))),
        Arc::new(TestBackend::new(key(3))),
    ];
    let superblock = create(temp.path(), &devices, Arc::new(TestCodec::default()));
    let mut transaction = futures_lite::future::block_on(superblock.begin_write()).expect("begin");
    for raw in [3, 1, 2] {
        futures_lite::future::block_on(
            transaction.write_on(key(raw), Bytes::from(vec![raw as u8])),
        )
        .expect("write");
    }
    futures_lite::future::block_on(transaction.commit()).expect("commit");
    for device in &devices {
        let calls = device.calls.lock().expect("calls");
        assert_eq!(calls.prepares, 1);
        assert_eq!(calls.drops, 1);
    }
}

#[test]
fn encode_and_prepare_failures_drop_batches_and_close_transaction() {
    let temp = tempdir().expect("tempdir");
    let device = Arc::new(TestBackend::new(key(1)));
    let codec = Arc::new(TestCodec::default());
    let superblock = create(temp.path(), std::slice::from_ref(&device), codec.clone());
    let mut encode =
        futures_lite::future::block_on(superblock.begin_write()).expect("begin encode");
    futures_lite::future::block_on(encode.write_on(key(1), Bytes::from_static(b"x")))
        .expect("write");
    encode.apply_application(TestDelta(1)).expect("delta");
    codec.set_fail_delta(true);
    assert!(matches!(
        futures_lite::future::block_on(encode.commit()),
        Err(SuperblockError::ApplicationCodec { .. })
    ));
    assert_eq!(device.calls.lock().expect("calls").drops, 1);

    codec.set_fail_delta(false);
    device.fail(Failure::Prepare);
    let mut flush = futures_lite::future::block_on(superblock.begin_write()).expect("begin flush");
    futures_lite::future::block_on(flush.write_on(key(1), Bytes::from_static(b"y")))
        .expect("write");
    assert!(matches!(
        futures_lite::future::block_on(flush.commit()),
        Err(SuperblockError::Device { source }) if source.operation == DeviceOperation::Prepare
    ));
    assert_eq!(device.calls.lock().expect("calls").drops, 2);
}

#[test]
fn closed_transaction_commands_have_zero_side_effects() {
    let temp = tempdir().expect("tempdir");
    let device = Arc::new(TestBackend::new(key(1)));
    let superblock = create(
        temp.path(),
        std::slice::from_ref(&device),
        Arc::new(TestCodec::default()),
    );
    let mut transaction = futures_lite::future::block_on(superblock.begin_write()).expect("begin");
    futures_lite::future::block_on(transaction.commit()).expect("commit");
    let before = device.calls.lock().expect("calls").writes;
    assert!(matches!(
        transaction.alloc_on(key(1), 1),
        Err(SuperblockError::TransactionClosed)
    ));
    assert!(matches!(
        futures_lite::future::block_on(transaction.write_on(key(1), Bytes::from_static(b"x"))),
        Err(SuperblockError::TransactionClosed)
    ));
    assert!(matches!(
        transaction.apply_application(TestDelta(9)),
        Err(SuperblockError::TransactionClosed)
    ));
    assert!(matches!(
        transaction.release(Vec::new()),
        Err(SuperblockError::TransactionClosed)
    ));
    assert_eq!(device.calls.lock().expect("calls").writes, before);
}

#[test]
fn release_large_block_is_durable_and_vector_prevalidation_is_atomic() {
    let temp = tempdir().expect("tempdir");
    let device = Arc::new(TestBackend::new(key(1)));
    let codec = Arc::new(TestCodec::default());
    let superblock = create(temp.path(), std::slice::from_ref(&device), codec.clone());
    let mut transaction = futures_lite::future::block_on(superblock.begin_write()).expect("begin");
    let live =
        futures_lite::future::block_on(transaction.write_on(key(1), Bytes::from_static(b"large")))
            .expect("large write");
    let invalid = BlockRef {
        device: key(1),
        block: BlockId::large(pi_block_device::LargeBlockId::new(999).expect("large id")),
    };
    assert!(matches!(
        transaction.release(vec![live, invalid]),
        Err(SuperblockError::InvalidBlockRef)
    ));
    futures_lite::future::block_on(transaction.commit()).expect("allocate commit");

    let mut release = futures_lite::future::block_on(superblock.begin_write()).expect("release");
    release.release(vec![live]).expect("release live block");
    futures_lite::future::block_on(release.commit()).expect("release commit");
    drop(transaction);
    drop(release);
    drop(superblock);

    let reopened = open(temp.path(), std::slice::from_ref(&device), codec);
    let mut next = futures_lite::future::block_on(reopened.begin_write()).expect("reopen");
    assert!(matches!(
        next.release(vec![live]),
        Err(SuperblockError::InvalidBlockRef)
    ));
}
