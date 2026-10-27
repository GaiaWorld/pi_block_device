//! Private DeviceTable implementation; public contracts remain in the parent.

use super::*;

impl DeviceTable {
    pub(super) fn new_impl(slots: Vec<DeviceSlot>) -> Result<Self, crate::CreateError> {
        let invalid = || crate::CreateError::NotCreated {
            kind: crate::CreateFailureKind::InvalidConfiguration,
            source: None,
        };
        if slots.is_empty() || slots.len() > 1_000_000 {
            return Err(invalid());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut labels = std::collections::BTreeSet::new();
        for slot in &slots {
            if !slot.state.validate()
                || !ids.insert(slot.registration.id)
                || !labels.insert(&slot.registration.label)
            {
                return Err(invalid());
            }
        }
        let mut registrations = Vec::with_capacity(slots.len());
        let mut states = VectorSync::new_sync();
        for slot in slots {
            registrations.push(slot.registration);
            states.push_back_mut(slot.state);
        }
        Ok(Self {
            registrations: registrations.into(),
            states,
        })
    }

    pub(super) fn len_impl(&self) -> usize {
        self.registrations.len()
    }

    pub(super) fn is_empty_impl(&self) -> bool {
        self.registrations.is_empty()
    }

    pub(super) fn get_impl(&self, key: DeviceKey) -> Option<DeviceSlotRef<'_>> {
        let index = usize::try_from(key.raw().checked_sub(1)?).ok()?;
        Some(DeviceSlotRef {
            registration: self.registrations.get(index)?,
            state: self.states.get(index)?,
        })
    }

    pub(crate) fn state_mut(
        &mut self,
        key: DeviceKey,
    ) -> Option<&mut pi_block_device::BlockDeviceState> {
        let index = usize::try_from(key.raw().checked_sub(1)?).ok()?;
        self.registrations.get(index)?;
        self.states.get_mut(index)
    }

    pub(super) fn iter_impl(&self) -> impl Iterator<Item = DeviceSlotRef<'_>> + '_ {
        self.registrations
            .iter()
            .zip(self.states.iter())
            .map(|(registration, state)| DeviceSlotRef {
                registration,
                state,
            })
    }
}
