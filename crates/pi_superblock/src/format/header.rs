//! Fixed eight-byte superblock record header.

use crate::SuperblockError;

const MAGIC: [u8; 4] = *b"PISB";
const SYSTEM_VERSION: u16 = 1;
pub(crate) const HEADER_LEN: usize = 8;

/// Persistent superblock record kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordKind {
    /// Complete checkpoint record.
    Checkpoint,
    /// Ordered transaction delta record.
    Delta,
}

impl RecordKind {
    fn to_byte(self) -> u8 {
        match self {
            Self::Checkpoint => 1,
            Self::Delta => 2,
        }
    }

    fn from_byte(value: u8) -> Result<Self, HeaderDecodeError> {
        match value {
            1 => Ok(Self::Checkpoint),
            2 => Ok(Self::Delta),
            _ => Err(HeaderDecodeError::UnknownKind),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HeaderDecodeError {
    UnsupportedVersion,
    InvalidState,
    UnknownKind,
}

impl std::fmt::Display for HeaderDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion => formatter.write_str("unsupported system version"),
            Self::InvalidState => formatter.write_str("invalid header state"),
            Self::UnknownKind => formatter.write_str("unknown record kind"),
        }
    }
}

impl std::error::Error for HeaderDecodeError {}

impl From<HeaderDecodeError> for SuperblockError {
    fn from(error: HeaderDecodeError) -> Self {
        match error {
            HeaderDecodeError::UnknownKind => Self::UnknownRecordKind,
            HeaderDecodeError::UnsupportedVersion | HeaderDecodeError::InvalidState => {
                Self::InvalidState
            }
        }
    }
}

/// Strictly decoded persistent record header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordHeader {
    /// Frozen wire magic.
    pub magic: [u8; 4],
    /// Frozen wire system version.
    pub system_version: u16,
    /// Record kind.
    pub record_kind: RecordKind,
    /// Frozen wire flags.
    pub flags: u8,
}

/// Encodes the frozen eight-byte record header.
pub fn encode_record_header(kind: RecordKind) -> Result<[u8; HEADER_LEN], SuperblockError> {
    let mut bytes = [0_u8; HEADER_LEN];
    bytes[0..4].copy_from_slice(&MAGIC);
    bytes[4..6].copy_from_slice(&SYSTEM_VERSION.to_le_bytes());
    bytes[6] = kind.to_byte();
    bytes[7] = 0;
    Ok(bytes)
}

pub(crate) fn decode_record_header_detailed(
    bytes: &[u8],
) -> Result<RecordHeader, HeaderDecodeError> {
    let bytes: &[u8; HEADER_LEN] = bytes
        .try_into()
        .map_err(|_| HeaderDecodeError::InvalidState)?;
    if bytes[0..4] != MAGIC || bytes[7] != 0 {
        return Err(HeaderDecodeError::InvalidState);
    }
    let system_version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if system_version > SYSTEM_VERSION {
        return Err(HeaderDecodeError::UnsupportedVersion);
    }
    if system_version != SYSTEM_VERSION {
        return Err(HeaderDecodeError::InvalidState);
    }
    let record_kind = RecordKind::from_byte(bytes[6])?;
    Ok(RecordHeader {
        magic: MAGIC,
        system_version: SYSTEM_VERSION,
        record_kind,
        flags: 0,
    })
}

/// Strictly decodes the frozen eight-byte record header.
pub fn decode_record_header(bytes: &[u8]) -> Result<RecordHeader, SuperblockError> {
    decode_record_header_detailed(bytes).map_err(Into::into)
}
