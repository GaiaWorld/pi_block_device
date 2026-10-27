//! Append-log recovery visitor and builder.

use pi_append_log::file::{DefaultFileLayout, FileAppendLogBuilder};
use pi_append_log::{
    AppendLogBuilder, AppendLogVisitor, BlockDecoder, BlockVisitContext, DefaultBlockCodec,
    ReadOrder,
};
use pi_block_device::RecordSeq;
use pi_result::{ClassifyErrorKind, InteropResultExt};
use std::sync::Arc;
use std::{io, path::PathBuf};

use crate::format::header::HeaderDecodeError;

use super::{SuperblockAppendLog, boxed_report, decode_append_log_block_detailed};

struct RecoveryVisitor<'a, S, C>
where
    S: crate::ApplicationState,
    C: crate::ApplicationCodec<S>,
{
    codec: &'a C,
    block_codec: &'a DefaultBlockCodec,
    previous_record_seq: Option<RecordSeq>,
    recovered: Option<crate::RecoveredState<S>>,
    pending_error: Option<crate::OpenError>,
    structure_count: usize,
}

impl<S, C> AppendLogVisitor for RecoveryVisitor<'_, S, C>
where
    S: crate::ApplicationState,
    C: crate::ApplicationCodec<S>,
{
    fn visit(&mut self, block: &[u8], context: BlockVisitContext) -> pi_result::Result<bool> {
        if context.is_first_in_structure {
            let Some(structure_count) = self.structure_count.checked_add(1) else {
                return Err(pi_result::error_stack::Report::new(
                    pi_result::ErrorKind::ResourceExhausted,
                ));
            };
            self.structure_count = structure_count;
        }
        let result: io::Result<bool> = (|| {
            let decoded = self
                .block_codec
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
            if record.header.record_kind == crate::RecordKind::Delta {
                let replay = (|| {
                    let recovered = self.recovered.as_mut().ok_or_else(|| {
                        recovery_error(
                            crate::RecoveryFailureKind::Decode,
                            crate::RecoveryOperation::DecodeDelta,
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "delta record requires a recovered checkpoint",
                            ),
                        )
                    })?;
                    let changes =
                        crate::decode_delta::<S, C>(record.body, self.codec).map_err(|error| {
                            recovery_error(
                                crate::RecoveryFailureKind::Decode,
                                crate::RecoveryOperation::DecodeDelta,
                                error,
                            )
                        })?;
                    let mut candidate = recovered.state.clone();
                    for (index, change) in changes.iter().enumerate() {
                        let index = u32::try_from(index).map_err(|error| {
                            recovery_error(
                                crate::RecoveryFailureKind::Decode,
                                crate::RecoveryOperation::DecodeDelta,
                                error,
                            )
                        })?;
                        match change {
                            crate::TransactionChange::System(change) => {
                                crate::change::apply_system_change(
                                    &mut candidate,
                                    change,
                                    record.record_seq,
                                )
                                .map_err(|error| {
                                    recovery_error(
                                        crate::RecoveryFailureKind::Decode,
                                        crate::RecoveryOperation::DecodeDelta,
                                        error,
                                    )
                                })?;
                            }
                            crate::TransactionChange::Application(delta) => {
                                let context = crate::ApplicationApplyContext::new(
                                    &candidate.devices,
                                    &candidate.roots,
                                    &candidate.bindings,
                                    record.record_seq,
                                    index,
                                );
                                candidate.application = candidate
                                    .application
                                    .apply(delta, &context)
                                    .map_err(|error| {
                                        recovery_error(
                                            crate::RecoveryFailureKind::ApplicationReplay,
                                            crate::RecoveryOperation::ReplayApplication,
                                            error,
                                        )
                                    })?;
                            }
                        }
                    }
                    if !candidate
                        .devices
                        .iter()
                        .all(|slot| slot.state().validate_at(record.record_seq))
                    {
                        return Err(recovery_error(
                            crate::RecoveryFailureKind::Decode,
                            crate::RecoveryOperation::DecodeDelta,
                            crate::SuperblockError::InvalidState,
                        ));
                    }
                    candidate.record_seq = record.record_seq;
                    recovered.state = candidate;
                    Ok::<(), crate::OpenError>(())
                })();
                if let Err(error) = replay {
                    self.pending_error = Some(error);
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "delta recovery failed",
                    ));
                }
                return Ok(false);
            }
            let state = crate::decode_checkpoint_state(record.record_seq, record.body, self.codec)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            self.recovered = Some(crate::RecoveredState {
                state,
                status: crate::SuperblockStatus::Ready,
            });
            Ok(false)
        })();
        result.into_error_by(|error| match error.kind() {
            io::ErrorKind::Unsupported => pi_result::ErrorKind::Unsupported,
            _ => error.classify_error_kind(),
        })
    }
}

fn recovery_error(
    kind: crate::RecoveryFailureKind,
    operation: crate::RecoveryOperation,
    source: impl std::error::Error + Send + Sync + 'static,
) -> crate::OpenError {
    crate::OpenError::RecoveryFailed {
        kind,
        context: crate::RecoveryErrorContext {
            device_key: None,
            operation,
            source: Box::new(source),
        },
    }
}

pub async fn build_recovering<S, C>(
    path: PathBuf,
    max_payload_len: usize,
    codec: &C,
) -> Result<(SuperblockAppendLog, crate::RecoveredState<S>), crate::OpenError>
where
    S: crate::ApplicationState,
    C: crate::ApplicationCodec<S>,
{
    let block_codec = DefaultBlockCodec::new(max_payload_len);
    let mut visitor = RecoveryVisitor {
        codec,
        block_codec: &block_codec,
        previous_record_seq: None,
        recovered: None,
        pending_error: None,
        structure_count: 0,
    };
    let build_result = FileAppendLogBuilder::new(path.clone(), DefaultFileLayout::new(path))
        .build(&block_codec, ReadOrder::Forward, &mut visitor)
        .await;
    if let Some(error) = visitor.pending_error.take() {
        return Err(error);
    }
    let build = build_result.map_err(|error| crate::OpenError::RecoveryFailed {
        kind: match error.classify_error_kind() {
            pi_result::ErrorKind::Corrupted => crate::RecoveryFailureKind::Decode,
            pi_result::ErrorKind::Unsupported => crate::RecoveryFailureKind::UnsupportedFormat,
            _ => crate::RecoveryFailureKind::LogStateIndeterminate,
        },
        context: crate::RecoveryErrorContext {
            device_key: None,
            operation: crate::RecoveryOperation::OpenLog,
            source: boxed_report(error),
        },
    })?;
    let structure_count = visitor.structure_count;
    let recovered = visitor.recovered.take();
    drop(visitor);
    let Some(mut recovered) = recovered else {
        drop(build.storage);
        return Err(crate::OpenError::NotInitialized);
    };
    recovered.status = if recovered.state.record_seq.next().is_err() {
        crate::SuperblockStatus::ReadOnlyExhausted
    } else {
        crate::SuperblockStatus::Ready
    };
    let recovered_closed: std::collections::VecDeque<_> =
        build.recovered_closed.into_iter().collect();
    let active_is_empty = !recovered_closed.is_empty() && structure_count == recovered_closed.len();
    let phase = if active_is_empty {
        super::LogMaintenancePhase::CheckpointRequired
    } else if recovered_closed.is_empty() {
        super::LogMaintenancePhase::Active
    } else {
        super::LogMaintenancePhase::ArchiveRetry
    };
    Ok((
        SuperblockAppendLog {
            storage: Arc::new(build.storage),
            codec: block_codec,
            append_options: pi_append_log::AppendOptions::default(),
            rotate_threshold: super::AtomicU64::new(u64::MAX),
            active_size: super::AtomicU64::new(0),
            phase: super::Mutex::new(phase),
            pending_archive: super::Mutex::new(recovered_closed),
            operation_gate: async_lock::Mutex::new(()),
            #[cfg(test)]
            archive_completed_hook: super::Mutex::new(None),
        },
        recovered,
    ))
}
