use std::error::Error;
use std::fmt;

use pi_block_device::{
    BlockDeviceBackend, BlockDeviceState, BoxError, DeviceKey, ReconcileError, ReconcileFailureKind,
};

use crate::append_log::{MaintenanceError, MaintenanceOutcome, SuperblockAppendLog};
use crate::{
    CreateError, CreateFailureKind, OpenError, RecoveryErrorContext, RecoveryFailureKind,
    RecoveryOperation, SuperblockError, SuperblockStatus,
};

use super::Superblock;
use super::SuperblockLogConfig;
use crate::{ApplicationCodec, ApplicationState, DeviceResolver};
use std::sync::Arc;

struct MaintenanceStatus<'a> {
    status: &'a crate::state::AtomicStatus,
    log: &'a SuperblockAppendLog,
    published: bool,
}

impl Drop for MaintenanceStatus<'_> {
    fn drop(&mut self) {
        if self.log.maintenance_phase() == crate::append_log::LogMaintenancePhase::RecoveryRequired
            || (!self.published
                && self.log.maintenance_phase()
                    == crate::append_log::LogMaintenancePhase::ArchivePending)
        {
            self.status.update(SuperblockStatus::RecoveryRequired);
        }
    }
}

impl<S, C, D, R> Superblock<S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    #[allow(dead_code)]
    pub(crate) async fn run_log_maintenance_internal(
        &self,
    ) -> Result<MaintenanceOutcome<S>, MaintenanceError> {
        ensure_writable_status(self.status())
            .map_err(|source| MaintenanceError::Rejected { source })?;
        let _writer_guard = self.writer_gate.lock().await;
        ensure_writable_status(self.status())
            .map_err(|source| MaintenanceError::Rejected { source })?;
        if !self.log.maintenance_pending() {
            return Ok(MaintenanceOutcome::NoOp {
                state: self.published.load_full(),
            });
        }
        let mut status_guard = MaintenanceStatus {
            status: &self.status,
            log: &self.log,
            published: true,
        };
        let result = self
            .log
            .run_log_maintenance(self.published.load_full(), self.codec.as_ref())
            .await?;
        match result {
            MaintenanceOutcome::Published {
                state,
                archived: false,
            } => {
                status_guard.published = false;
                {
                    let _publication = self.publication_access.lock().map_err(|_| {
                        MaintenanceError::RecoveryRequired {
                            source: Box::new(SuperblockError::RecoveryRequired),
                        }
                    })?;
                    self.published.store(Arc::clone(&state));
                    if state.record_seq.next().is_err() {
                        self.status.update(SuperblockStatus::ReadOnlyExhausted);
                    }
                }
                status_guard.published = true;
                // First archive still runs after the final checkpoint; retries are rejected at entry.
                self.log.run_archive(state).await
            }
            other => Ok(other),
        }
    }

    #[allow(dead_code)]
    pub(crate) async fn run_pending_maintenance_internal(
        &self,
    ) -> Result<MaintenanceOutcome<S>, MaintenanceError> {
        self.run_log_maintenance_internal().await
    }
}

#[derive(Debug)]
struct DeviceKeyIndexError {
    index: usize,
}

impl fmt::Display for DeviceKeyIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "device index {} cannot be represented as DeviceKey",
            self.index
        )
    }
}

impl Error for DeviceKeyIndexError {}

pub(super) fn ensure_writable_status(status: SuperblockStatus) -> Result<(), SuperblockError> {
    match status {
        SuperblockStatus::Ready => Ok(()),
        SuperblockStatus::ReadOnlyExhausted => Err(SuperblockError::ReadOnlyExhausted),
        SuperblockStatus::RecoveryRequired => Err(SuperblockError::RecoveryRequired),
    }
}

pub(super) async fn reconcile_with_retry<D: BlockDeviceBackend>(
    backend: &std::sync::Arc<D>,
    expected: std::sync::Arc<BlockDeviceState>,
) -> Result<(), ReconcileError> {
    let mut attempts = 0_u8;
    loop {
        attempts += 1;
        match backend.reconcile(expected.clone()).await {
            Ok(_) => return Ok(()),
            Err(error) if attempts < 3 && is_retryable(error.kind) => continue,
            Err(error) => return Err(error),
        }
    }
}

fn is_retryable(kind: ReconcileFailureKind) -> bool {
    matches!(
        kind,
        ReconcileFailureKind::RepairIo
            | ReconcileFailureKind::MediaMetadataSync
            | ReconcileFailureKind::ConcurrentMutation
    )
}

pub(super) fn validate_config(
    log: &SuperblockLogConfig,
    device_count: usize,
) -> Result<(), CreateError> {
    if device_count == 0 || log.rotate_threshold == 0 || device_count > 1_000_000 {
        return Err(CreateError::NotCreated {
            kind: CreateFailureKind::InvalidConfiguration,
            source: None,
        });
    }
    Ok(())
}

pub(super) fn create_device_key(index: usize) -> Result<DeviceKey, CreateError> {
    checked_device_key(index).map_err(|source| CreateError::NotCreated {
        kind: CreateFailureKind::InvalidConfiguration,
        source: Some(Box::new(source)),
    })
}

pub(super) fn open_device_key(index: usize) -> Result<DeviceKey, OpenError> {
    checked_device_key(index).map_err(|source| {
        recovery_error(
            None,
            RecoveryFailureKind::Decode,
            RecoveryOperation::DecodeCheckpoint,
            Box::new(source),
        )
    })
}

fn checked_device_key(index: usize) -> Result<DeviceKey, DeviceKeyIndexError> {
    let one_based = index.checked_add(1).ok_or(DeviceKeyIndexError { index })?;
    let raw = u32::try_from(one_based).map_err(|_| DeviceKeyIndexError { index })?;
    DeviceKey::new(raw).map_err(|_| DeviceKeyIndexError { index })
}

#[derive(Debug)]
pub(super) struct DeviceIdentityMismatchError;

impl fmt::Display for DeviceIdentityMismatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("device identity mismatch")
    }
}

impl Error for DeviceIdentityMismatchError {}

pub(super) fn recovery_error(
    key: Option<DeviceKey>,
    kind: RecoveryFailureKind,
    operation: RecoveryOperation,
    source: BoxError,
) -> OpenError {
    OpenError::RecoveryFailed {
        kind,
        context: RecoveryErrorContext {
            device_key: key,
            operation,
            source,
        },
    }
}

pub(super) fn map_create_codec(error: SuperblockError) -> CreateError {
    let (kind, source) = match error {
        SuperblockError::RecordTooLarge => (CreateFailureKind::RecordTooLarge, None),
        SuperblockError::ApplicationCodec { source } => (
            CreateFailureKind::ApplicationCodec,
            Some(Box::new(source) as BoxError),
        ),
        other => (
            CreateFailureKind::ApplicationValidation,
            Some(Box::new(other) as BoxError),
        ),
    };
    CreateError::NotCreated { kind, source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn production_config_count_boundaries_without_allocating_devices() {
        let mut log = SuperblockLogConfig {
            path: PathBuf::from("unused"),
            rotate_threshold: 1,
            append_log_options: pi_append_log::AppendOptions { durable: false },
        };
        for (count, valid) in [
            (0, false),
            (1, true),
            (999_999, true),
            (1_000_000, true),
            (1_000_001, false),
            (usize::MAX, false),
        ] {
            let result = validate_config(&log, count);
            assert_eq!(result.is_ok(), valid, "count={count}");
            if !valid {
                assert!(matches!(
                    result,
                    Err(CreateError::NotCreated {
                        kind: CreateFailureKind::InvalidConfiguration,
                        source: None
                    })
                ));
            }
        }
        log.rotate_threshold = 0;
        assert!(validate_config(&log, 1).is_err());
    }
}
