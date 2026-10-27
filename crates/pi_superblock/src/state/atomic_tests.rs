use super::{AtomicStatus, SuperblockStatus};
use std::sync::Arc;

#[test]
fn recovery_status_cannot_be_overwritten_by_concurrent_ready_or_exhausted() {
    let status = Arc::new(AtomicStatus::new(SuperblockStatus::Ready));
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = Arc::clone(&status);
    let thread = std::thread::spawn(move || {
        rx.recv().unwrap();
        worker.update(SuperblockStatus::Ready);
        worker.update(SuperblockStatus::ReadOnlyExhausted);
    });
    status.update(SuperblockStatus::RecoveryRequired);
    tx.send(()).unwrap();
    thread.join().unwrap();
    assert_eq!(status.load(), SuperblockStatus::RecoveryRequired);
    let exhausted = AtomicStatus::new(SuperblockStatus::ReadOnlyExhausted);
    exhausted.update(SuperblockStatus::Ready);
    assert_eq!(exhausted.load(), SuperblockStatus::ReadOnlyExhausted);
    assert_eq!(
        AtomicStatus(std::sync::atomic::AtomicU8::new(255)).load(),
        SuperblockStatus::RecoveryRequired
    );
}
