//! Private context queries, bounded encoding and validated names.

use super::*;

impl<'a> ApplicationApplyContext<'a> {
    pub(super) fn device_table_impl(&self) -> &DeviceTable {
        self.device_table
    }

    pub(super) fn device_state_impl(&self, key: DeviceKey) -> Option<&BlockDeviceState> {
        self.device_table.get(key).map(|slot| slot.state())
    }

    pub(super) fn roots_impl(&self) -> impl Iterator<Item = RootVersion> + '_ {
        self.roots
            .iter()
            .map(|(&block, &published_at)| RootVersion {
                block,
                published_at,
            })
    }

    pub(super) fn bindings_impl(&self) -> impl Iterator<Item = (&BindingName, BlockRef)> + '_ {
        self.bindings.iter().map(|(name, &block)| (name, block))
    }
}

impl ApplicationEncoder {
    pub(super) fn remaining_impl(&self) -> usize {
        self.hard_limit.saturating_sub(self.buffer.len())
    }

    pub(super) fn reserve(&mut self, length: usize) -> Result<(), ApplicationCodecError> {
        if length > self.remaining() {
            return Err(ApplicationCodecError::limit_exceeded(
                ApplicationCodecOperation::EncoderWrite,
            ));
        }
        self.buffer.reserve(length);
        Ok(())
    }

    pub(super) fn put_slice_impl(&mut self, bytes: &[u8]) -> Result<(), ApplicationCodecError> {
        self.reserve(bytes.len())?;
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    pub(super) fn put_u8_impl(&mut self, value: u8) -> Result<(), ApplicationCodecError> {
        self.put_slice(&[value])
    }

    pub(super) fn put_u16_le_impl(&mut self, value: u16) -> Result<(), ApplicationCodecError> {
        self.put_slice(&value.to_le_bytes())
    }

    pub(super) fn put_u32_le_impl(&mut self, value: u32) -> Result<(), ApplicationCodecError> {
        self.put_slice(&value.to_le_bytes())
    }

    pub(super) fn put_u64_le_impl(&mut self, value: u64) -> Result<(), ApplicationCodecError> {
        self.put_slice(&value.to_le_bytes())
    }

    pub(super) fn freeze_impl(self) -> Bytes {
        self.buffer.freeze()
    }
}

impl DeviceLabel {
    pub(super) fn new_impl(value: String) -> Result<Self, crate::CreateError> {
        if value.is_empty() || value.len() > 255 {
            return Err(crate::CreateError::NotCreated {
                kind: crate::CreateFailureKind::InvalidConfiguration,
                source: None,
            });
        }
        Ok(Self(value.into_boxed_str()))
    }

    pub(super) fn as_bytes_impl(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl BindingName {
    pub(super) fn new_impl(bytes: Vec<u8>) -> Result<Self, SuperblockError> {
        if bytes.is_empty() || bytes.len() > MAX_NAME_BYTES {
            return Err(SuperblockError::InvalidBindingName);
        }
        let value = String::from_utf8(bytes).map_err(|_| SuperblockError::InvalidBindingName)?;
        Ok(Self(value.into_boxed_str()))
    }

    pub(super) fn as_bytes_impl(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

pub(super) fn default_apply<S: ApplicationState>(
    context: &ApplicationApplyContext<'_>,
) -> Result<S, ApplicationApplyError> {
    Err(ApplicationApplyError::not_implemented(context.change_index))
}
