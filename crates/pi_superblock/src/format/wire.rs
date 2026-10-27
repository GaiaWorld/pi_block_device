//! Checked primitives for decoding persistent wire values.

use crate::SuperblockError;

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn remaining_bytes(&self) -> &'a [u8] {
        &self.bytes[self.position..]
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], SuperblockError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(SuperblockError::InvalidBlockRef)?;
        let result = self
            .bytes
            .get(self.position..end)
            .ok_or(SuperblockError::InvalidBlockRef)?;
        self.position = end;
        Ok(result)
    }

    pub(crate) fn bytes(&mut self, length: usize) -> Result<&'a [u8], SuperblockError> {
        self.take(length)
    }

    pub(crate) fn array_12(&mut self) -> Result<[u8; 12], SuperblockError> {
        self.take(12)?
            .try_into()
            .map_err(|_| SuperblockError::InvalidBlockRef)
    }

    pub(crate) fn array_16(&mut self) -> Result<[u8; 16], SuperblockError> {
        self.take(16)?
            .try_into()
            .map_err(|_| SuperblockError::InvalidBlockRef)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, SuperblockError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(SuperblockError::InvalidBlockRef)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, SuperblockError> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| SuperblockError::InvalidBlockRef)?,
        ))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, SuperblockError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| SuperblockError::InvalidBlockRef)?,
        ))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, SuperblockError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| SuperblockError::InvalidBlockRef)?,
        ))
    }
}
