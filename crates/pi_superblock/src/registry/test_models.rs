//! Private log-test model; never polls the unfinished registry constructor.
use super::*;

#[derive(Clone)]
pub(crate) struct LogState;
impl crate::ApplicationState for LogState {
    type Delta = ();
}

pub(crate) fn state() -> Arc<crate::SuperblockState<LogState>> {
    Arc::new(crate::SuperblockState {
        record_seq: pi_block_device::RecordSeq::new(1).unwrap(),
        devices: DeviceTable {
            registrations: Arc::from([]),
            states: VectorSync::new_sync(),
        },
        roots: rpds::RedBlackTreeMapSync::new_sync(),
        bindings: rpds::RedBlackTreeMapSync::new_sync(),
        application: LogState,
    })
}
