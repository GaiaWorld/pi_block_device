//! @impl @api: BlockDeviceBackend::reconcile
//! @design-doc: design/features/04-reconcile-retire.feature
use crate::FileBlockDevice;
use futures_lite::StreamExt;
use pi_async_fs::{FileAccessMode, FileFlushMode, FileIo, FileNamespace};
use pi_block_device::{
    BlockDeviceState, BlockId, LargeBlockId, ReconcileError, ReconcileFailureKind,
    ReconcileProgress, ReconcileReport, RetiredObject, SegmentVersion,
};
use std::collections::BTreeMap;
use std::sync::Arc;

struct RecoveryAdmission<'a>(&'a FileBlockDevice);
impl Drop for RecoveryAdmission<'_> {
    fn drop(&mut self) {
        self.0
            .recovering
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

pub(super) async fn reconcile(
    device: &Arc<FileBlockDevice>,
    expected: Arc<BlockDeviceState>,
) -> Result<ReconcileReport, ReconcileError> {
    if !expected.validate() {
        return Err(failure(
            ReconcileFailureKind::ConcurrentMutation,
            0,
            0,
            "invalid expected state",
        ));
    }
    let _round = device.reclaim_gate.lock().await;
    let _writes = device.write_gate.lock().await;
    let was_fenced = device
        .write_fenced
        .swap(true, std::sync::atomic::Ordering::AcqRel);
    // 与 lease 注册使用同一准入锁；取消只释放围栏，不执行异步补偿。
    {
        let _gate = device
            .retirement_gate
            .lock()
            .map_err(|error| failure(ReconcileFailureKind::ConcurrentMutation, 0, 0, error))?;
        device
            .recovering
            .store(true, std::sync::atomic::Ordering::Release);
    }
    let _admission = RecoveryAdmission(device);
    let mut required = BTreeMap::new();
    for (level, versions) in expected.levels.iter().enumerate() {
        for (offset, entry) in versions.versions.iter().enumerate() {
            let Some(data) = entry else { continue };
            if data.released_at.is_empty() {
                continue;
            }
            let version = SegmentVersion::new(versions.min_version.raw() + offset as u32).map_err(
                |error| failure(ReconcileFailureKind::RequiredObjectCorrupt, 0, 0, error),
            )?;
            let block = BlockId::ordinary(level as u8, version, 0).map_err(|error| {
                failure(ReconcileFailureKind::RequiredObjectCorrupt, 0, 0, error)
            })?;
            let length = (data.released_at.len() as u64)
                .checked_mul(u64::from(
                    pi_block_device::BackendCapabilities::fixed().level_sizes[level],
                ))
                .ok_or_else(|| {
                    failure(
                        ReconcileFailureKind::RequiredObjectCorrupt,
                        0,
                        0,
                        "segment length overflow",
                    )
                })?;
            let active = offset + 1 == versions.versions.len();
            required.insert(device.block_path(block), (length, active));
        }
    }
    for (id, data) in expected.large_blocks.entries.iter() {
        required.insert(
            device.block_path(BlockId::large(*id)),
            (data.logical_len, false),
        );
    }
    let mut stream = device
        .namespace
        .read_dir(device.root.clone())
        .await
        .map_err(|error| failure(ReconcileFailureKind::RepairIo, 0, 0, error))?;
    let mut observed = BTreeMap::new();
    let mut orphans = Vec::new();
    let mut inspected_objects = 0;
    while let Some(entry) = stream.next().await {
        let entry = entry.map_err(|error| failure(ReconcileFailureKind::RepairIo, 0, 0, error))?;
        inspected_objects += 1;
        if managed_object(&entry.locator).is_some() {
            observed.insert(entry.locator.clone(), entry.locator);
        } else if is_temporary(&entry.locator) {
            orphans.push(entry.locator);
        }
    }
    let mut repairs = Vec::new();
    for (path, (length, active)) in &required {
        if !observed.contains_key(path) {
            return Err(failure(
                ReconcileFailureKind::RequiredObjectMissing,
                0,
                0,
                path.display(),
            ));
        }
        let file = device
            .namespace
            .open(path, FileAccessMode::Read)
            .await
            .map_err(|error| {
                io_failure(ReconcileFailureKind::RequiredObjectMissing, 0, 0, error)
            })?;
        let actual = file.byte_len().await.map_err(|error| {
            io_failure(ReconcileFailureKind::RequiredObjectCorrupt, 0, 0, error)
        })?;
        if actual > *length && *active {
            let object = managed_object(path).ok_or_else(|| {
                failure(
                    ReconcileFailureKind::RequiredObjectCorrupt,
                    0,
                    0,
                    "invalid required path",
                )
            })?;
            let states = device
                .snapshot_states
                .lock()
                .map_err(|error| failure(ReconcileFailureKind::ConcurrentMutation, 0, 0, error))?;
            if states
                .iter()
                .filter_map(std::sync::Weak::upgrade)
                .any(|state| crate::reclaim_impl::state_references(&state, object))
            {
                return Err(failure(
                    ReconcileFailureKind::ConcurrentMutation,
                    0,
                    0,
                    "snapshot protects required tail",
                ));
            }
            repairs.push((path.clone(), *length, actual));
        } else if actual != *length {
            return Err(failure(
                ReconcileFailureKind::RequiredObjectCorrupt,
                0,
                0,
                format!("{} length {actual}, expected {length}", path.display()),
            ));
        }
    }
    orphans.extend(
        observed
            .into_keys()
            .filter(|path| !required.contains_key(path)),
    );
    let planned = (orphans.len() + repairs.len()) as u64;
    let mut completed = 0;
    for (path, length, observed_len) in repairs {
        let mut file = device
            .namespace
            .open(&path, FileAccessMode::Truncate)
            .await
            .map_err(|error| {
                io_failure(ReconcileFailureKind::RepairIo, planned, completed, error)
            })?;
        if file.byte_len().await.map_err(|error| {
            io_failure(ReconcileFailureKind::RepairIo, planned, completed, error)
        })? != observed_len
        {
            return Err(failure(
                ReconcileFailureKind::ConcurrentMutation,
                planned,
                completed,
                "required tail changed before repair",
            ));
        }
        // 仅丢弃权威 Active 分配边界之外的未提交后缀，绝不填补缺失前缀。
        // 依赖的排他 truncate 租约还会拒绝任何活动或正在建立的映射。
        file.truncate(length).await.map_err(|error| {
            io_failure(ReconcileFailureKind::RepairIo, planned, completed, error)
        })?;
        #[cfg(test)]
        crate::write_cancel_tests::pause(device, true).await;
        file.flush(FileFlushMode::DataAndMetadata)
            .await
            .map_err(|error| {
                io_failure(
                    ReconcileFailureKind::MediaMetadataSync,
                    planned,
                    completed,
                    error,
                )
            })?;
        completed += 1;
    }
    for path in orphans {
        let mut reusable_admission = None;
        if let Some(object) = managed_object(&path) {
            if let RetiredObject::Segment { level, version } = object {
                let versions = &expected.levels[usize::from(level)];
                let offset = version.raw().checked_sub(versions.min_version.raw());
                let virtual_active = offset.is_some_and(|offset| {
                    offset as usize + 1 == versions.versions.len()
                        && versions.versions.get(offset as usize).is_some_and(|entry| {
                            entry
                                .as_ref()
                                .is_some_and(|data| data.released_at.is_empty())
                        })
                });
                // 只记录本轮新准入的当前虚拟 Active。既有退休/取消准入永不解除。
                // reclaim/write gate 及 recovering 阻止此检查与准入之间的新写/lease/回收。
                if virtual_active
                    && !device
                        .retirement_gate
                        .lock()
                        .map_err(|error| {
                            failure(
                                ReconcileFailureKind::ConcurrentMutation,
                                planned,
                                completed,
                                error,
                            )
                        })?
                        .contains(&object)
                {
                    reusable_admission = Some(object);
                }
            }
            if !crate::reclaim_impl::admit_delete(device, object).map_err(|error| {
                failure(
                    ReconcileFailureKind::ConcurrentMutation,
                    planned,
                    completed,
                    error,
                )
            })? {
                return Err(failure(
                    ReconcileFailureKind::ConcurrentMutation,
                    planned,
                    completed,
                    "snapshot protects orphan",
                ));
            }
        }
        #[cfg(test)]
        crate::write_cancel_tests::pause(device, false).await;
        match device.namespace.remove_file(&path).await {
            Ok(()) => completed += 1,
            Err(error)
                if *error.target_evidence()
                    == pi_async_fs::RemoveTargetEvidence::RemovedByOperation =>
            {
                completed += 1
            }
            Err(error) => {
                if !matches!(
                    error.target_evidence(),
                    pi_async_fs::RemoveTargetEvidence::NotRemovedByOperation
                ) {
                    if let Some(object) = managed_object(&path) {
                        device
                            .unresolved_removals
                            .lock()
                            .map_err(|error| {
                                failure(
                                    ReconcileFailureKind::ConcurrentMutation,
                                    planned,
                                    completed,
                                    error,
                                )
                            })?
                            .push(object);
                    }
                }
                return Err(io_failure(
                    ReconcileFailureKind::RepairIo,
                    planned,
                    completed,
                    error,
                ));
            }
        }
        // 已 await 删除的明确完成，结果载体与底层操作已收尾；不能以取消或不存在猜测完成。
        #[cfg(test)]
        crate::write_cancel_tests::pause(device, true).await;
        if let Some(object) = reusable_admission {
            device
                .retirement_gate
                .lock()
                .map_err(|error| {
                    failure(
                        ReconcileFailureKind::ConcurrentMutation,
                        planned,
                        completed,
                        error,
                    )
                })?
                .retain(|admitted| *admitted != object);
        }
    }
    // 新故障模型仍要求真实文件 sync：包括所有 required 对象及身份 manifest。
    // namespace 操作的完成不代表父目录 fsync，也不承诺目录项抗突然掉电。
    for path in required
        .keys()
        .chain(std::iter::once(&device.root.join("manifest.bin")))
    {
        let mut file = device
            .namespace
            .open(path, FileAccessMode::Append)
            .await
            .map_err(|error| {
                io_failure(
                    ReconcileFailureKind::MediaMetadataSync,
                    planned,
                    completed,
                    error,
                )
            })?;
        file.flush(FileFlushMode::DataAndMetadata)
            .await
            .map_err(|error| {
                io_failure(
                    ReconcileFailureKind::MediaMetadataSync,
                    planned,
                    completed,
                    error,
                )
            })?;
    }
    device
        .write_fenced
        .store(was_fenced, std::sync::atomic::Ordering::Release);
    Ok(ReconcileReport {
        inspected_objects,
        progress: ReconcileProgress {
            planned_actions: planned,
            completed_actions: completed,
            remaining_actions: 0,
            partial_progress: false,
            media_metadata_synced: true,
        },
    })
}

fn io_failure<T: std::fmt::Debug + std::fmt::Display + Send + Sync + 'static>(
    kind: ReconcileFailureKind,
    planned: u64,
    completed: u64,
    source: T,
) -> ReconcileError {
    let mut error = failure(kind, planned, completed, "file I/O failed");
    error.source = Some(Box::new(crate::write_impl::IoFailure(source)));
    error
}

fn managed_object(path: &std::path::Path) -> Option<RetiredObject> {
    let name = path.file_name()?.to_str()?;
    if let Some(raw) = name
        .strip_prefix("large-")
        .and_then(|name| name.strip_suffix(".bin"))
    {
        let id = LargeBlockId::new(u64::from_str_radix(raw, 16).ok()?).ok()?;
        return (name == format!("large-{:015x}.bin", id.raw()))
            .then_some(RetiredObject::LargeBlock { id });
    }
    let raw = name.strip_prefix("segment-")?.strip_suffix(".bin")?;
    let (level, version) = raw.split_once('-')?;
    let level = u8::from_str_radix(level, 16).ok()?;
    let version = SegmentVersion::new(u32::from_str_radix(version, 16).ok()?).ok()?;
    (level < 15 && name == format!("segment-{level:02x}-{:08x}.bin", version.raw()))
        .then_some(RetiredObject::Segment { level, version })
}

fn is_temporary(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.len() == ".write-0000000000000000.tmp".len()
                && name.starts_with(".write-")
                && name.ends_with(".tmp")
                && name[7..23]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn failure(
    kind: ReconcileFailureKind,
    planned: u64,
    completed: u64,
    source: impl std::fmt::Display,
) -> ReconcileError {
    ReconcileError {
        kind,
        progress: ReconcileProgress {
            planned_actions: planned,
            completed_actions: completed,
            remaining_actions: planned.saturating_sub(completed),
            partial_progress: completed > 0,
            media_metadata_synced: false,
        },
        source: Some(Box::new(std::io::Error::other(source.to_string()))),
    }
}
