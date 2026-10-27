use super::{Error, FileDeviceError, fmt};
use pi_async_fs::{CreateFailure, RenameFailure};

// CreateFailure is not std::error::Error; retain its report and evidence intact.
#[derive(Debug)]
pub(super) struct CreateError {
    operation: &'static str,
    pub(super) failure: CreateFailure,
}

impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pi_async_fs {} failed: {}", self.operation, self.failure)
    }
}

impl Error for CreateError {}

pub(super) fn wrap(operation: &'static str, failure: CreateFailure) -> FileDeviceError {
    FileDeviceError::Io {
        source: Box::new(CreateError { operation, failure }),
    }
}

#[derive(Debug)]
pub(super) struct PublishError {
    pub(super) failure: RenameFailure,
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pi_async_fs publish manifest failed: {}", self.failure)
    }
}

impl Error for PublishError {}

pub(super) fn wrap_rename(failure: RenameFailure) -> FileDeviceError {
    FileDeviceError::Io {
        source: Box::new(PublishError { failure }),
    }
}
