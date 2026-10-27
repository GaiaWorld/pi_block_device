use std::error::Error;
use std::fmt;

use pi_block_device::{BackendCapabilities, DeviceKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Capability field used in a structured mismatch diagnostic.
pub enum CapabilityField {
    ProtocolVersion,
    BaseBlockSize,
    LevelSizes,
    IndependentLargeBlocks,
    OptionalFlags,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Lossless diagnostic representation of a capability value.
pub enum CapabilityValue {
    U16(u16),
    U32(u32),
    Levels([u32; 15]),
    Bool(bool),
    U64(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Structured required-versus-actual backend capability mismatch.
pub struct CapabilityMismatchError {
    pub device_key: DeviceKey,
    pub field: CapabilityField,
    pub required: CapabilityValue,
    pub actual: CapabilityValue,
}

impl fmt::Display for CapabilityMismatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "device {} capability {:?} mismatch: required {:?}, actual {:?}",
            self.device_key.raw(),
            self.field,
            self.required,
            self.actual
        )
    }
}

impl Error for CapabilityMismatchError {}

pub(crate) fn compare_capabilities(
    device_key: DeviceKey,
    required: &BackendCapabilities,
    actual: &BackendCapabilities,
) -> Result<(), Box<CapabilityMismatchError>> {
    let mismatch = if required.protocol_version != actual.protocol_version {
        Some((
            CapabilityField::ProtocolVersion,
            CapabilityValue::U16(required.protocol_version),
            CapabilityValue::U16(actual.protocol_version),
        ))
    } else if required.base_block_size != actual.base_block_size {
        Some((
            CapabilityField::BaseBlockSize,
            CapabilityValue::U32(required.base_block_size),
            CapabilityValue::U32(actual.base_block_size),
        ))
    } else if required.level_sizes != actual.level_sizes {
        Some((
            CapabilityField::LevelSizes,
            CapabilityValue::Levels(required.level_sizes),
            CapabilityValue::Levels(actual.level_sizes),
        ))
    } else if required.independent_large_blocks != actual.independent_large_blocks {
        Some((
            CapabilityField::IndependentLargeBlocks,
            CapabilityValue::Bool(required.independent_large_blocks),
            CapabilityValue::Bool(actual.independent_large_blocks),
        ))
    } else if (actual.optional_flags & required.optional_flags) != required.optional_flags {
        Some((
            CapabilityField::OptionalFlags,
            CapabilityValue::U64(required.optional_flags),
            CapabilityValue::U64(actual.optional_flags),
        ))
    } else {
        None
    };
    match mismatch {
        Some((field, required, actual)) => Err(Box::new(CapabilityMismatchError {
            device_key,
            field,
            required,
            actual,
        })),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_extra_bits_are_accepted() {
        let mut required = BackendCapabilities::fixed();
        required.optional_flags = 1;
        let mut actual = required.clone();
        actual.optional_flags = 3;
        assert!(compare_capabilities(DeviceKey::new(1).unwrap(), &required, &actual).is_ok());
    }

    #[test]
    fn optional_missing_bits_are_structured_mismatch() {
        let mut required = BackendCapabilities::fixed();
        required.optional_flags = 3;
        let mut actual = required.clone();
        actual.optional_flags = 1;
        let error =
            compare_capabilities(DeviceKey::new(1).unwrap(), &required, &actual).unwrap_err();
        assert_eq!(error.device_key, DeviceKey::new(1).unwrap());
        assert_eq!(error.field, CapabilityField::OptionalFlags);
        assert_eq!(error.required, CapabilityValue::U64(3));
        assert_eq!(error.actual, CapabilityValue::U64(1));
    }

    #[test]
    fn optional_zero_required_accepts_nonzero_actual() {
        let required = BackendCapabilities::fixed();
        assert_eq!(required.optional_flags, 0);
        let mut actual = required.clone();
        actual.optional_flags = u64::MAX;
        assert!(compare_capabilities(DeviceKey::new(1).unwrap(), &required, &actual).is_ok());
    }
}
