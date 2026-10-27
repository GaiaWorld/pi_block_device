use std::sync::Arc;

use pi_append_log::{AppendLog, AppendOptions};
use pi_block_device::{BoxError, RecordSeq};

use crate::{ApplicationCodec, ApplicationState, SuperblockError};

use super::{DurableAppendOutcome, LogMaintenancePhase, SuperblockAppendLog, boxed_report};
#[cfg(test)]
mod resource_tests;

#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
#[derive(Debug)]
#[doc(hidden)]
pub enum MaintenanceError {
    Rejected { source: SuperblockError },
    RecoveryRequired { source: BoxError },
}

#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
#[doc(hidden)]
pub enum MaintenanceOutcome<S: ApplicationState> {
    NoOp {
        state: Arc<crate::SuperblockState<S>>,
    },
    Published {
        state: Arc<crate::SuperblockState<S>>,
        archived: bool,
    },
    ArchiveRetryPending {
        state: Arc<crate::SuperblockState<S>>,
    },
}

// 操作入口持日志 operation_gate；上层 writer gate 另保护业务 State/发布整程。
struct ArchiveRound<'a> {
    queue: &'a std::sync::Mutex<std::collections::VecDeque<super::ClosedStructure>>,
    remaining: std::collections::VecDeque<super::ClosedStructure>,
}

impl Drop for ArchiveRound<'_> {
    fn drop(&mut self) {
        // archive is idempotent for ArchiveOnly; cancellation can safely retain responsibility.
        let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
        self.remaining.append(&mut queue);
        *queue = std::mem::take(&mut self.remaining);
    }
}

struct MaintenanceAttempt<'a> {
    log: &'a SuperblockAppendLog,
    armed: bool,
}

impl Drop for MaintenanceAttempt<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.log.set_phase(LogMaintenancePhase::RecoveryRequired);
        }
    }
}

impl SuperblockAppendLog {
    #[doc(hidden)]
    pub async fn run_log_maintenance<S, C>(
        &self,
        current: Arc<crate::SuperblockState<S>>,
        codec: &C,
    ) -> Result<MaintenanceOutcome<S>, MaintenanceError>
    where
        S: ApplicationState,
        C: ApplicationCodec<S>,
    {
        let _operation = self.operation_gate.lock().await;
        self.run_log_maintenance_unlocked(current, codec).await
    }

    async fn run_log_maintenance_unlocked<S, C>(
        &self,
        current: Arc<crate::SuperblockState<S>>,
        codec: &C,
    ) -> Result<MaintenanceOutcome<S>, MaintenanceError>
    where
        S: ApplicationState,
        C: ApplicationCodec<S>,
    {
        if current.record_seq.next().is_err() {
            return Err(MaintenanceError::Rejected {
                source: SuperblockError::ReadOnlyExhausted,
            });
        }
        let phase = self.maintenance_phase();
        match phase {
            LogMaintenancePhase::Active => {
                if self.active_size.load(super::Ordering::Acquire)
                    < self.rotate_threshold.load(super::Ordering::Acquire)
                {
                    return Ok(MaintenanceOutcome::NoOp { state: current });
                }
                self.set_phase(LogMaintenancePhase::RotateRequested);
            }
            LogMaintenancePhase::ArchivePending | LogMaintenancePhase::ArchiveRetry => {
                return self.run_archive_unlocked(current).await;
            }
            LogMaintenancePhase::RecoveryRequired | LogMaintenancePhase::Rotating => {
                return Err(MaintenanceError::RecoveryRequired {
                    source: Box::new(SuperblockError::RecoveryRequired),
                });
            }
            LogMaintenancePhase::RotateRequested | LogMaintenancePhase::CheckpointRequired => {}
        }
        let next = current
            .record_seq
            .next()
            .map_err(|_| MaintenanceError::Rejected {
                source: SuperblockError::ReadOnlyExhausted,
            })?;
        let body = crate::encode_checkpoint(
            next,
            &current.devices,
            &current.roots,
            &current.bindings,
            &current.application,
            codec,
        )
        .map_err(|source| MaintenanceError::Rejected { source })?;
        let mut attempt = MaintenanceAttempt {
            log: self,
            armed: true,
        };
        if phase == LogMaintenancePhase::CheckpointRequired {
            if self
                .pending_archive
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
            {
                return Err(MaintenanceError::RecoveryRequired {
                    source: Box::new(SuperblockError::RecoveryRequired),
                });
            }
        } else {
            self.set_phase(LogMaintenancePhase::Rotating);
            match self.storage.rotate().await {
                Ok(Some(closed)) => {
                    self.pending_archive
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push_back(closed);
                    self.active_size.store(0, super::Ordering::Release);
                    self.set_phase(LogMaintenancePhase::CheckpointRequired);
                }
                Ok(None) => {
                    attempt.armed = false;
                    self.set_phase(LogMaintenancePhase::Active);
                    return Ok(MaintenanceOutcome::NoOp { state: current });
                }
                Err(source) => {
                    return Err(MaintenanceError::RecoveryRequired {
                        source: boxed_report(source),
                    });
                }
            }
        }
        match self
            .append_durable_unlocked(next, crate::RecordKind::Checkpoint, body)
            .await
        {
            DurableAppendOutcome::Durable { record_seq, .. } => {
                let mut candidate = (*current).clone();
                candidate.record_seq = record_seq;
                self.set_phase(LogMaintenancePhase::ArchivePending);
                attempt.armed = false;
                Ok(MaintenanceOutcome::Published {
                    state: Arc::new(candidate),
                    archived: false,
                })
            }
            DurableAppendOutcome::NotAppended { source }
            | DurableAppendOutcome::Unknown { source } => {
                Err(MaintenanceError::RecoveryRequired { source })
            }
        }
    }

    #[doc(hidden)]
    pub async fn run_archive<S: ApplicationState>(
        &self,
        current: Arc<crate::SuperblockState<S>>,
    ) -> Result<MaintenanceOutcome<S>, MaintenanceError> {
        let _operation = self.operation_gate.lock().await;
        self.run_archive_unlocked(current).await
    }

    async fn run_archive_unlocked<S: ApplicationState>(
        &self,
        current: Arc<crate::SuperblockState<S>>,
    ) -> Result<MaintenanceOutcome<S>, MaintenanceError> {
        match self.maintenance_phase() {
            LogMaintenancePhase::Active | LogMaintenancePhase::RotateRequested => {
                return Ok(MaintenanceOutcome::NoOp { state: current });
            }
            LogMaintenancePhase::ArchivePending | LogMaintenancePhase::ArchiveRetry => {}
            LogMaintenancePhase::Rotating
            | LogMaintenancePhase::CheckpointRequired
            | LogMaintenancePhase::RecoveryRequired => {
                return Err(MaintenanceError::RecoveryRequired {
                    source: Box::new(SuperblockError::RecoveryRequired),
                });
            }
        }
        let remaining = std::mem::take(
            &mut *self
                .pending_archive
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        let mut round = ArchiveRound {
            queue: &self.pending_archive,
            remaining,
        };
        if round.remaining.is_empty() {
            self.set_phase(LogMaintenancePhase::Active);
            return Ok(MaintenanceOutcome::NoOp { state: current });
        }
        // Fixed snapshot: a failed item stays owned by the guard and is not attempted twice.
        while let Some(closed) = round.remaining.front() {
            if self.storage.archive(closed.clone()).await.is_err() {
                self.set_phase(LogMaintenancePhase::ArchiveRetry);
                return Ok(MaintenanceOutcome::ArchiveRetryPending { state: current });
            }
            #[cfg(test)]
            {
                let hook = self.archive_completed_hook.lock().unwrap().take();
                if let Some(entered) = hook {
                    entered.send(()).unwrap();
                    // 已执行真实 Local archive，但尚未消费责任；模拟此处取消。
                    std::future::pending::<()>().await;
                }
            }
            round.remaining.pop_front();
        }
        drop(round);
        if self
            .pending_archive
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
        {
            self.set_phase(LogMaintenancePhase::Active);
            Ok(MaintenanceOutcome::Published {
                state: current,
                archived: true,
            })
        } else {
            self.set_phase(LogMaintenancePhase::ArchiveRetry);
            Ok(MaintenanceOutcome::ArchiveRetryPending { state: current })
        }
    }
}

pub(super) async fn append_confirmed<F, Fut>(
    record_seq: RecordSeq,
    encoded: Vec<u8>,
    mut options: AppendOptions,
    append: F,
) -> DurableAppendOutcome
where
    F: FnOnce(Vec<u8>, AppendOptions) -> Fut,
    Fut: std::future::Future<Output = pi_result::Result<u64>>,
{
    options.durable = true;
    match append(encoded, options).await {
        Ok(active_size) => DurableAppendOutcome::Durable {
            record_seq,
            active_size,
        },
        Err(error) => DurableAppendOutcome::Unknown {
            source: boxed_report(error),
        },
    }
}
