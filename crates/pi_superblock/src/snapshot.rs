mod implementation;

use std::future::Future;
use std::sync::Arc;

use pi_block_device::{BlockDeviceBackend, BlockDeviceState, BlockRef, DeviceKey, RecordSeq};

use crate::{
    ApplicationState, BindingName, DeviceRegistration, RootVersion, SuperblockError,
    SuperblockState, SuperblockStatus,
};

/// A fixed logical superblock view with one backend lease per registered device.
pub struct SuperblockSnapshot<S: ApplicationState, D: BlockDeviceBackend> {
    state: Arc<SuperblockState<S>>,
    devices: Vec<Arc<D>>,
    leases: Vec<D::SnapshotLease>,
    status: Arc<crate::state::AtomicStatus>,
}

impl<S: ApplicationState, D: BlockDeviceBackend> SuperblockSnapshot<S, D> {
    pub(crate) fn new(
        state: Arc<SuperblockState<S>>,
        devices: Vec<Arc<D>>,
        leases: Vec<D::SnapshotLease>,
        status: Arc<crate::state::AtomicStatus>,
    ) -> Self {
        Self {
            state,
            devices,
            leases,
            status,
        }
    }
}

/// Approved read-only queries over a fixed superblock snapshot.
pub trait SuperblockSnapshotApi<S: ApplicationState, D: BlockDeviceBackend> {
    fn record_seq(&self) -> RecordSeq;
    fn application(&self) -> &S;
    fn devices(&self) -> impl Iterator<Item = (DeviceKey, &DeviceRegistration)> + '_;
    fn device_state(&self, key: DeviceKey) -> Option<&BlockDeviceState>;
    fn get_root(&self, block: BlockRef) -> Option<RootVersion>;
    fn roots(&self) -> impl Iterator<Item = RootVersion> + '_;
    fn get_binding(&self, name: &BindingName) -> Option<BlockRef>;
    fn bindings(&self) -> impl Iterator<Item = (&BindingName, BlockRef)> + '_;
    #[allow(clippy::manual_async_fn)]
    fn read(
        &self,
        block: BlockRef,
    ) -> impl Future<Output = Result<D::ReadBuf, SuperblockError>> + Send + '_;
}

impl<S: ApplicationState, D: BlockDeviceBackend> SuperblockSnapshotApi<S, D>
    for SuperblockSnapshot<S, D>
{
    fn record_seq(&self) -> RecordSeq {
        self.record_seq_impl()
    }
    fn application(&self) -> &S {
        self.application_impl()
    }
    fn devices(&self) -> impl Iterator<Item = (DeviceKey, &DeviceRegistration)> + '_ {
        self.devices_impl()
    }
    fn device_state(&self, _key: DeviceKey) -> Option<&BlockDeviceState> {
        self.device_state_impl(_key)
    }
    fn get_root(&self, block: BlockRef) -> Option<RootVersion> {
        self.get_root_impl(block)
    }
    fn roots(&self) -> impl Iterator<Item = RootVersion> + '_ {
        self.roots_impl()
    }
    fn get_binding(&self, name: &BindingName) -> Option<BlockRef> {
        self.get_binding_impl(name)
    }
    fn bindings(&self) -> impl Iterator<Item = (&BindingName, BlockRef)> + '_ {
        self.bindings_impl()
    }
    #[allow(clippy::manual_async_fn)]
    fn read(
        &self,
        block: BlockRef,
    ) -> impl Future<Output = Result<D::ReadBuf, SuperblockError>> + Send + '_ {
        self.read_impl(block)
    }
}
