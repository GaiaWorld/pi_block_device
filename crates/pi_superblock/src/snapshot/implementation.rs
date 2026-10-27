//! Private SuperblockSnapshot implementation; public contracts remain in the parent.

use super::*;
use pi_block_device::DeviceOperation;

impl<S: ApplicationState, D: BlockDeviceBackend> SuperblockSnapshot<S, D> {
    pub(super) fn record_seq_impl(&self) -> RecordSeq {
        self.state.record_seq
    }

    pub(super) fn application_impl(&self) -> &S {
        &self.state.application
    }

    pub(super) fn devices_impl(
        &self,
    ) -> impl Iterator<Item = (DeviceKey, &DeviceRegistration)> + '_ {
        (1_u32..)
            .zip(self.state.devices.iter())
            .filter_map(|(raw, slot)| {
                DeviceKey::new(raw)
                    .ok()
                    .map(|key| (key, slot.registration()))
            })
    }

    pub(super) fn device_state_impl(&self, key: DeviceKey) -> Option<&BlockDeviceState> {
        self.state.devices.get(key).map(|slot| slot.state())
    }

    pub(super) fn get_root_impl(&self, block: BlockRef) -> Option<RootVersion> {
        self.state
            .roots
            .get(&block)
            .copied()
            .map(|published_at| RootVersion {
                block,
                published_at,
            })
    }

    pub(super) fn roots_impl(&self) -> impl Iterator<Item = RootVersion> + '_ {
        self.state
            .roots
            .iter()
            .map(|(&block, &published_at)| RootVersion {
                block,
                published_at,
            })
    }

    pub(super) fn get_binding_impl(&self, name: &BindingName) -> Option<BlockRef> {
        self.state.bindings.get(name).copied()
    }

    pub(super) fn bindings_impl(&self) -> impl Iterator<Item = (&BindingName, BlockRef)> + '_ {
        self.state
            .bindings
            .iter()
            .map(|(name, &block)| (name, block))
    }

    pub(super) async fn read_impl(&self, block: BlockRef) -> Result<D::ReadBuf, SuperblockError> {
        self.reads_allowed()?;
        if !crate::state::is_live_block(&self.state.devices, block, self.state.record_seq) {
            return Err(SuperblockError::InvalidBlockRef);
        }
        let index = Self::device_index(block.device)
            .filter(|&index| index < self.devices.len() && index < self.leases.len())
            .ok_or(SuperblockError::InvalidBlockRef)?;
        self.devices[index]
            .read(&self.leases[index], block.block)
            .await
            .map_err(|mut source| {
                source.operation = DeviceOperation::Read;
                source.device_key = Some(block.device);
                SuperblockError::Device { source }
            })
    }

    fn device_index(key: DeviceKey) -> Option<usize> {
        usize::try_from(key.raw().checked_sub(1)?).ok()
    }

    fn reads_allowed(&self) -> Result<(), SuperblockError> {
        match self.status.load() {
            SuperblockStatus::RecoveryRequired => Err(SuperblockError::RecoveryRequired),
            SuperblockStatus::Ready | SuperblockStatus::ReadOnlyExhausted => Ok(()),
        }
    }
}
