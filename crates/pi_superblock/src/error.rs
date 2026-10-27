//! Public errors for the first superblock implementation slice.

use std::error::Error;
use std::fmt;

use pi_block_device::BoxError;

/// Stable recovery stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryOperation {
    OpenLog,
    DecodeCheckpoint,
    DecodeDelta,
    ReplayApplication,
    Resolve,
    ValidateIdentity,
    ValidateCapabilities,
    Reconcile,
}

/// Stable recovery failure classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryFailureKind {
    UnsupportedFormat,
    Decode,
    ApplicationFormatMismatch,
    ApplicationReplay,
    ResolveDevice,
    DeviceIdentityMismatch,
    IncompatibleDeviceCapabilities,
    Reconcile,
    LogStateIndeterminate,
}

/// Structured recovery failure context.
#[derive(Debug)]
pub struct RecoveryErrorContext {
    pub device_key: Option<pi_block_device::DeviceKey>,
    pub operation: RecoveryOperation,
    pub source: BoxError,
}

/// Error returned while opening and recovering a superblock.
#[derive(Debug)]
pub enum OpenError {
    NotInitialized,
    RecoveryFailed {
        kind: RecoveryFailureKind,
        context: RecoveryErrorContext,
    },
}

impl fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => formatter.write_str("superblock is not initialized"),
            Self::RecoveryFailed { kind, context } => write!(
                formatter,
                "superblock recovery failed during {:?} ({kind:?}): {}",
                context.operation, context.source
            ),
        }
    }
}

impl Error for OpenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotInitialized => None,
            Self::RecoveryFailed { context, .. } => Some(context.source.as_ref()),
        }
    }
}

/// Invalid append-log block sequence for the superblock record range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidBlockSequence;

/// Stable application codec operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationCodecOperation {
    FormatId,
    Version,
    EncodeState,
    DecodeState,
    EncodeDelta,
    DecodeDelta,
    EncoderWrite,
}

/// Stable application codec failure classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationCodecFailureKind {
    InvalidFormatId,
    InvalidVersion,
    Encode,
    Decode,
    LimitExceeded,
    NonEmptyPayload,
}

#[derive(Debug)]
struct ApplicationCodecMessage(String);

impl fmt::Display for ApplicationCodecMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ApplicationCodecMessage {}

#[derive(Debug)]
struct ApplicationApplyMessage;

impl fmt::Display for ApplicationApplyMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("application apply is not implemented")
    }
}

impl Error for ApplicationApplyMessage {}

/// Application delta apply failure.
#[derive(Debug)]
pub struct ApplicationApplyError {
    pub change_index: u32,
    pub source: BoxError,
}

impl ApplicationApplyError {
    pub(crate) fn not_implemented(change_index: u32) -> Self {
        Self {
            change_index,
            source: Box::new(ApplicationApplyMessage),
        }
    }
}

impl fmt::Display for ApplicationApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "application change {} failed: {}",
            self.change_index, self.source
        )
    }
}

impl Error for ApplicationApplyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Application codec failure.
#[derive(Debug)]
pub struct ApplicationCodecError {
    pub operation: ApplicationCodecOperation,
    pub reason: ApplicationCodecFailureKind,
    pub source: Option<BoxError>,
}

impl ApplicationCodecError {
    /// Creates a structured codec error with an optional underlying source.
    pub fn with_source(
        operation: ApplicationCodecOperation,
        reason: ApplicationCodecFailureKind,
        source: Option<BoxError>,
    ) -> Self {
        Self {
            operation,
            reason,
            source,
        }
    }

    /// Creates a structured codec error whose source preserves the supplied message.
    pub fn from_message(
        operation: ApplicationCodecOperation,
        reason: ApplicationCodecFailureKind,
        message: impl Into<String>,
    ) -> Self {
        Self::with_source(
            operation,
            reason,
            Some(Box::new(ApplicationCodecMessage(message.into()))),
        )
    }

    /// Legacy compatibility constructor.
    ///
    /// This defaults to [`ApplicationCodecOperation::EncodeState`] and
    /// [`ApplicationCodecFailureKind::Encode`]. New code should use
    /// [`Self::with_source`] or [`Self::from_message`] to provide accurate context.
    pub fn new(message: impl Into<String>) -> Self {
        Self::from_message(
            ApplicationCodecOperation::EncodeState,
            ApplicationCodecFailureKind::Encode,
            message,
        )
    }

    pub(crate) fn limit_exceeded(operation: ApplicationCodecOperation) -> Self {
        Self::with_source(operation, ApplicationCodecFailureKind::LimitExceeded, None)
    }
}

impl fmt::Display for ApplicationCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Some(source) => source.fmt(formatter),
            None => write!(
                formatter,
                "application codec {:?} failed: {:?}",
                self.operation, self.reason
            ),
        }
    }
}

impl Error for ApplicationCodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Stable create failure categories reserved for the full constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateFailureKind {
    InvalidConfiguration,
    DeviceInspection,
    DeviceNotEmpty,
    IncompatibleDeviceCapabilities,
    ApplicationCodec,
    ApplicationValidation,
    RecordTooLarge,
    LogPreparation,
    AppendNotStarted,
}

/// Error returned by the future superblock constructor.
#[derive(Debug)]
pub enum CreateError {
    NotCreated {
        kind: CreateFailureKind,
        source: Option<BoxError>,
    },
    StatusUnknown {
        source: BoxError,
    },
}

/// First-slice logical state error.
#[derive(Debug)]
pub enum SuperblockError {
    /// A block reference or state relation is invalid.
    InvalidBlockRef,
    /// The encoded record header is structurally invalid.
    InvalidState,
    /// The record kind is not supported by this implementation.
    UnknownRecordKind,
    /// A binding name is invalid.
    InvalidBindingName,
    /// An application codec operation failed.
    ApplicationCodec { source: ApplicationCodecError },
    /// Applying an application delta failed.
    ApplicationApply { source: ApplicationApplyError },
    /// The record is too large.
    RecordTooLarge,
    /// The record sequence is exhausted.
    RecordSeqExhausted,
    /// The transaction is closed.
    TransactionClosed,
    /// The superblock is read-only because the sequence is exhausted.
    ReadOnlyExhausted,
    /// Recovery is required before further writes.
    RecoveryRequired,
    /// A block-device participant operation failed.
    Device {
        source: pi_block_device::DeviceError,
    },
    /// Append did not start.
    CommitFailed,
    /// Append outcome cannot be determined.
    CommitStatusUnknown,
}

impl fmt::Display for SuperblockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBlockRef => formatter.write_str("invalid block reference"),
            Self::InvalidState => formatter.write_str("invalid state"),
            Self::UnknownRecordKind => formatter.write_str("unknown record kind"),
            Self::InvalidBindingName => formatter.write_str("invalid binding name"),
            Self::ApplicationCodec { source } => {
                write!(formatter, "application codec failed: {source}")
            }
            Self::ApplicationApply { source } => {
                write!(formatter, "application apply failed: {source}")
            }
            Self::RecordTooLarge => formatter.write_str("record too large"),
            Self::RecordSeqExhausted => formatter.write_str("record sequence exhausted"),
            Self::TransactionClosed => formatter.write_str("transaction closed"),
            Self::ReadOnlyExhausted => formatter.write_str("superblock is read-only"),
            Self::RecoveryRequired => formatter.write_str("recovery required"),
            Self::Device { source } => write!(formatter, "device operation failed: {source}"),
            Self::CommitFailed => formatter.write_str("commit failed"),
            Self::CommitStatusUnknown => formatter.write_str("commit status unknown"),
        }
    }
}

impl Error for SuperblockError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ApplicationCodec { source } => Some(source),
            Self::ApplicationApply { source } => Some(source),
            Self::Device { source } => Some(source),
            _ => None,
        }
    }
}
