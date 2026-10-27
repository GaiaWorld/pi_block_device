//! Internal append-log record adaptation.

use bytes::Bytes;
use pi_append_log::file::{DefaultFileLayout, FileAppendLog, FileAppendLogBuilder};
use pi_append_log::{
    AppendLog, AppendLogBuilder, AppendLogVisitor, AppendOptions, BlockDecoder, BlockVisitContext,
    DefaultBlockCodec, ReadOrder,
};
use pi_block_device::{BoxError, RecordSeq};
use pi_result::{ClassifyErrorKind, InteropResultExt};
use std::io;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::SuperblockError;
use crate::format::header::HeaderDecodeError;

mod codec;
#[cfg(test)]
mod durable_tests;
mod maintenance;
#[cfg(test)]
mod overlap_tests;
mod recovery;
#[cfg(test)]
mod shared_tests;
pub use codec::DecodedSuperblockRecord;
pub use codec::decode_append_log_block;
pub(super) use codec::decode_append_log_block_detailed;
pub use codec::encode_append_log_block;
pub use maintenance::MaintenanceError;
pub use maintenance::MaintenanceOutcome;
use maintenance::append_confirmed;
pub use recovery::build_recovering;

#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LogMaintenancePhase {
    Active,
    RotateRequested,
    Rotating,
    CheckpointRequired,
    ArchivePending,
    ArchiveRetry,
    RecoveryRequired,
}

type FileStorage = FileAppendLog<DefaultFileLayout>;
pub(crate) type ClosedStructure = <FileStorage as AppendLog>::Closed;

/// Result of one durable append attempt.
#[doc(hidden)]
#[derive(Debug)]
pub enum DurableAppendOutcome {
    NotAppended {
        source: BoxError,
    },
    Durable {
        record_seq: RecordSeq,
        active_size: u64,
    },
    Unknown {
        source: BoxError,
    },
}

/// Concrete internal adapter around the file append log.
#[doc(hidden)]
pub struct SuperblockAppendLog {
    pub(super) storage: Arc<FileStorage>,
    pub(super) codec: DefaultBlockCodec,
    append_options: AppendOptions,
    rotate_threshold: AtomicU64,
    active_size: AtomicU64,
    phase: Mutex<LogMaintenancePhase>,
    pending_archive: Mutex<VecDeque<ClosedStructure>>,
    // 所有共享异步入口同一日志操作锁；保护多步状态与取消归还，不是上层 writer 锁。
    operation_gate: async_lock::Mutex<()>,
    #[cfg(test)]
    archive_completed_hook: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

#[derive(Debug)]
struct SourceError {
    source: BoxError,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for SourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
struct PiResultError {
    error: pi_result::Error,
}

impl std::fmt::Display for PiResultError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for PiResultError {}

pub(super) fn boxed_report(error: pi_result::Error) -> BoxError {
    Box::new(SourceError {
        source: Box::new(PiResultError { error }),
    })
}

#[derive(Default)]
struct EmptyVisitor {
    saw_history: bool,
}

impl AppendLogVisitor for EmptyVisitor {
    fn visit(&mut self, _block: &[u8], _context: BlockVisitContext) -> pi_result::Result<bool> {
        self.saw_history = true;
        Ok(true)
    }
}

struct ReplayVisitor<'a, F> {
    codec: &'a DefaultBlockCodec,
    previous_record_seq: Option<RecordSeq>,
    delivery_stopped: bool,
    saw_record: bool,
    visit: F,
}

impl<F> AppendLogVisitor for ReplayVisitor<'_, F>
where
    F: FnMut(DecodedSuperblockRecord) -> bool,
{
    fn visit(&mut self, block: &[u8], context: BlockVisitContext) -> pi_result::Result<bool> {
        self.saw_record = true;
        let result: io::Result<bool> = (|| {
            let decoded = self
                .codec
                .decode_forward(block)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if decoded.encoded_len() != block.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "trailing bytes after append-log block",
                ));
            }
            let record = decode_append_log_block_detailed(decoded).map_err(|error| {
                let kind = match error {
                    HeaderDecodeError::UnsupportedVersion => io::ErrorKind::Unsupported,
                    HeaderDecodeError::InvalidState | HeaderDecodeError::UnknownKind => {
                        io::ErrorKind::InvalidData
                    }
                };
                io::Error::new(kind, error)
            })?;
            if context.is_first_in_structure
                && record.header.record_kind != crate::RecordKind::Checkpoint
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "append-log structure must start with a checkpoint record",
                ));
            }
            if self
                .previous_record_seq
                .is_some_and(|previous| record.record_seq <= previous)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "append-log record sequence must be strictly increasing",
                ));
            }
            self.previous_record_seq = Some(record.record_seq);
            if !self.delivery_stopped && (self.visit)(record) {
                self.delivery_stopped = true;
            }
            Ok(false)
        })();
        result.into_error_by(|error| match error.kind() {
            io::ErrorKind::Unsupported => pi_result::ErrorKind::Unsupported,
            _ => error.classify_error_kind(),
        })
    }
}

impl SuperblockAppendLog {
    /// Temporary TDD seam for opening the concrete append log.
    #[doc(hidden)]
    pub async fn open(path: PathBuf, max_payload_len: usize) -> Result<Self, SuperblockError> {
        Self::open_configured(path, max_payload_len, AppendOptions::default())
            .await
            .map_err(|_| SuperblockError::InvalidState)
    }

    pub(crate) async fn open_configured(
        path: PathBuf,
        max_payload_len: usize,
        append_options: AppendOptions,
    ) -> Result<Self, BoxError> {
        let codec = DefaultBlockCodec::new(max_payload_len);
        let mut visitor = EmptyVisitor::default();
        let build = FileAppendLogBuilder::new(path.clone(), DefaultFileLayout::new(path))
            .build(&codec, ReadOrder::Forward, &mut visitor)
            .await
            .map_err(boxed_report)?;
        if visitor.saw_history {
            return Err(Box::new(SuperblockError::InvalidState));
        }
        Ok(Self {
            storage: Arc::new(build.storage),
            codec,
            append_options,
            rotate_threshold: AtomicU64::new(u64::MAX),
            active_size: AtomicU64::new(0),
            phase: Mutex::new(LogMaintenancePhase::Active),
            pending_archive: Mutex::new(VecDeque::new()),
            operation_gate: async_lock::Mutex::new(()),
            #[cfg(test)]
            archive_completed_hook: Mutex::new(None),
        })
    }

    /// Temporary TDD seam for streaming recovered records to a caller-owned sink.
    #[doc(hidden)]
    pub async fn open_replaying<F>(
        path: PathBuf,
        max_payload_len: usize,
        visit: F,
    ) -> Result<Self, crate::OpenError>
    where
        F: FnMut(DecodedSuperblockRecord) -> bool + Send,
    {
        let codec = DefaultBlockCodec::new(max_payload_len);
        let mut visitor = ReplayVisitor {
            codec: &codec,
            previous_record_seq: None,
            delivery_stopped: false,
            saw_record: false,
            visit,
        };
        let build = FileAppendLogBuilder::new(path.clone(), DefaultFileLayout::new(path))
            .build(&codec, ReadOrder::Forward, &mut visitor)
            .await
            .map_err(|error| crate::OpenError::RecoveryFailed {
                kind: match error.classify_error_kind() {
                    pi_result::ErrorKind::Corrupted => crate::RecoveryFailureKind::Decode,
                    pi_result::ErrorKind::Unsupported => {
                        crate::RecoveryFailureKind::UnsupportedFormat
                    }
                    _ => crate::RecoveryFailureKind::LogStateIndeterminate,
                },
                context: crate::RecoveryErrorContext {
                    device_key: None,
                    operation: crate::RecoveryOperation::OpenLog,
                    source: boxed_report(error),
                },
            })?;
        let saw_record = visitor.saw_record;
        drop(visitor);
        if !saw_record {
            drop(build.storage);
            return Err(crate::OpenError::NotInitialized);
        }
        Ok(Self {
            storage: Arc::new(build.storage),
            codec,
            append_options: AppendOptions::default(),
            rotate_threshold: AtomicU64::new(u64::MAX),
            active_size: AtomicU64::new(0),
            phase: Mutex::new(LogMaintenancePhase::Active),
            pending_archive: Mutex::new(VecDeque::new()),
            operation_gate: async_lock::Mutex::new(()),
            #[cfg(test)]
            archive_completed_hook: Mutex::new(None),
        })
    }

    pub(crate) fn set_append_options(&mut self, append_options: AppendOptions) {
        self.append_options = append_options;
    }

    #[doc(hidden)]
    pub fn set_rotate_threshold(&self, rotate_threshold: u64) {
        self.rotate_threshold
            .store(rotate_threshold, Ordering::Release);
    }

    pub(crate) fn maintenance_pending(&self) -> bool {
        !self
            .pending_archive
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
            || self.maintenance_phase() != LogMaintenancePhase::Active
            || self.active_size.load(Ordering::Acquire)
                >= self.rotate_threshold.load(Ordering::Acquire)
    }

    #[cfg_attr(not(feature = "test-support"), allow(dead_code))]
    pub(crate) fn maintenance_phase(&self) -> LogMaintenancePhase {
        self.phase
            .lock()
            .map(|phase| *phase)
            .unwrap_or(LogMaintenancePhase::RecoveryRequired)
    }

    pub(super) fn set_phase(&self, next: LogMaintenancePhase) {
        let mut phase = self.phase.lock().unwrap_or_else(|error| error.into_inner());
        if *phase != LogMaintenancePhase::RecoveryRequired {
            *phase = next;
        }
    }

    /// Temporary TDD seam for one durable append.
    #[doc(hidden)]
    pub async fn append_durable(
        &self,
        record_seq: RecordSeq,
        kind: crate::RecordKind,
        body: Bytes,
    ) -> DurableAppendOutcome {
        let _operation = self.operation_gate.lock().await;
        self.append_durable_unlocked(record_seq, kind, body).await
    }

    // 仅在持 operation_gate 的入口/维护芯内调用，避免递归加锁。
    async fn append_durable_unlocked(
        &self,
        record_seq: RecordSeq,
        kind: crate::RecordKind,
        body: Bytes,
    ) -> DurableAppendOutcome {
        match self.maintenance_phase() {
            LogMaintenancePhase::RecoveryRequired | LogMaintenancePhase::Rotating => {
                return DurableAppendOutcome::NotAppended {
                    source: Box::new(SuperblockError::RecoveryRequired),
                };
            }
            LogMaintenancePhase::CheckpointRequired if kind != crate::RecordKind::Checkpoint => {
                return DurableAppendOutcome::NotAppended {
                    source: Box::new(SuperblockError::RecoveryRequired),
                };
            }
            _ => {}
        }
        let encoded = match encode_append_log_block(&self.codec, record_seq, kind, &body) {
            Ok(encoded) => encoded,
            Err(error) => {
                return DurableAppendOutcome::NotAppended {
                    source: Box::new(error),
                };
            }
        };
        let outcome = append_confirmed(
            record_seq,
            encoded,
            self.append_options,
            |encoded, options| self.storage.append(encoded, options),
        )
        .await;
        if let DurableAppendOutcome::Durable { active_size, .. } = outcome {
            self.active_size.fetch_max(active_size, Ordering::AcqRel);
            let mut phase = self.phase.lock().unwrap_or_else(|error| error.into_inner());
            if self.active_size.load(Ordering::Acquire)
                >= self.rotate_threshold.load(Ordering::Acquire)
                && *phase == LogMaintenancePhase::Active
            {
                *phase = LogMaintenancePhase::RotateRequested;
            }
        }
        outcome
    }
}
