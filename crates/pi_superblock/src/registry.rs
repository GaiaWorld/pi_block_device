//! Static device registry values.

mod implementation;
#[cfg(test)]
pub(crate) mod test_models;

use pi_block_device::{BlockDeviceId, DeviceKey};
use rpds::VectorSync;
use std::sync::Arc;

use crate::DeviceLabel;

/// Immutable device identity stored in checkpoints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRegistration {
    /// Stable backend identity.
    pub id: BlockDeviceId,
    /// Stable human-readable label.
    pub label: DeviceLabel,
}

/// One immutable logical device slot stored in checkpoints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSlot {
    registration: DeviceRegistration,
    state: pi_block_device::BlockDeviceState,
}

impl DeviceSlot {
    /// Creates a slot from immutable registration and logical state.
    pub fn new(registration: DeviceRegistration, state: pi_block_device::BlockDeviceState) -> Self {
        Self {
            registration,
            state,
        }
    }

    /// Returns the immutable registration.
    pub fn registration(&self) -> &DeviceRegistration {
        &self.registration
    }

    /// Returns the logical allocation state.
    pub fn state(&self) -> &pi_block_device::BlockDeviceState {
        &self.state
    }
}

/// Borrowed view of matching static registration and persistent logical state.
#[derive(Clone, Copy)]
pub struct DeviceSlotRef<'a> {
    registration: &'a DeviceRegistration,
    state: &'a pi_block_device::BlockDeviceState,
}
impl<'a> DeviceSlotRef<'a> {
    pub fn registration(&self) -> &'a DeviceRegistration {
        self.registration
    }
    pub fn state(&self) -> &'a pi_block_device::BlockDeviceState {
        self.state
    }
}

/// Static registrations and structurally shared dynamic states.
/// Both collections must have equal lengths; DeviceKey is one-based input order.
/// Mutating states must not clone registrations or use Arc::make_mut on slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceTable {
    registrations: Arc<[DeviceRegistration]>,
    states: VectorSync<pi_block_device::BlockDeviceState>,
}

impl DeviceTable {
    /// Builds a non-empty table and assigns keys by one-based input order.
    pub fn new(slots: Vec<DeviceSlot>) -> Result<Self, crate::CreateError> {
        Self::new_impl(slots)
    }

    /// Returns the number of static slots.
    pub fn len(&self) -> usize {
        self.len_impl()
    }

    /// Returns whether the table has no slots.
    pub fn is_empty(&self) -> bool {
        self.is_empty_impl()
    }

    /// Returns a borrowed registration/state view at the checked one-based key.
    pub fn get(&self, key: DeviceKey) -> Option<DeviceSlotRef<'_>> {
        self.get_impl(key)
    }

    /// Iterates slots in stable DeviceKey order.
    pub fn iter(&self) -> impl Iterator<Item = DeviceSlotRef<'_>> + '_ {
        self.iter_impl()
    }
}
