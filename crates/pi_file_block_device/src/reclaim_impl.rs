//! @impl @api: BlockDeviceBackend::reclaim_retirements
//! @design-doc: design/features/04-reconcile-retire.feature
use super::*;

pub(super) fn enqueue(
    device: &Arc<FileBlockDevice>,
    objects: Vec<RetiredObject>,
) -> Result<(), DeviceError> {
    let mut queue = device
        .retirements
        .lock()
        .map_err(|_| error(DeviceOperation::EnqueueRetirements))?;
    for object in objects {
        if matches!(object, RetiredObject::Segment { level: 15.., .. }) {
            return Err(dev_err(
                DeviceOperation::EnqueueRetirements,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid ordinary level",
                )),
            ));
        }
        if !queue.contains(&object) {
            queue.push_back(object);
        }
    }
    Ok(())
}

pub(super) async fn reclaim(
    device: &Arc<FileBlockDevice>,
) -> Result<RetirementReport, DeviceError> {
    let _round = device.reclaim_gate.lock().await;
    // 取得资格时固定对象身份，不让后来入队延长本轮；I/O 期间责任仍在队列中。
    let selected: Vec<_> = device
        .retirements
        .lock()
        .map_err(|_| error(DeviceOperation::ReclaimRetirements))?
        .iter()
        .copied()
        .collect();
    let mut report = RetirementReport {
        attempted: 0,
        completed: 0,
        guarded: 0,
        retryable: 0,
    };
    if selected.is_empty() {
        return Ok(report);
    }
    // 空轮不等待活跃 batch；非空范围已冻结，等待期间入队留给下一轮。
    let _writes = device.write_gate.lock().await;
    #[cfg(test)]
    crate::retirement_tests::pause(device, false).await;
    for object in selected {
        report.attempted += 1;
        if device
            .unresolved_removals
            .lock()
            .map_err(|_| error(DeviceOperation::ReclaimRetirements))?
            .contains(&object)
        {
            report.retryable += 1;
            continue;
        }
        if !admit_delete(device, object)? {
            report.guarded += 1;
            continue;
        }
        let block = match object {
            RetiredObject::Segment { level, version } => BlockId::ordinary(level, version, 0)
                .map_err(|source| dev_err(DeviceOperation::ReclaimRetirements, Box::new(source)))?,
            RetiredObject::LargeBlock { id } => BlockId::large(id),
        };
        // 成功准入后不再授予此身份新 lease。取消也不撤销记录：底层删除
        // 可能继续；后续轮次仅在依赖明确证据下完成，ID 不复用。
        let path = device.block_path(block);
        #[cfg(test)]
        crate::retirement_tests::pause(device, true).await;
        let completed = match device.namespace.remove_file(&path).await {
            Ok(()) => true,
            Err(failure) => {
                let (source, evidence) = failure.into_parts();
                if !matches!(
                    evidence,
                    pi_async_fs::RemoveTargetEvidence::RemovedByOperation
                        | pi_async_fs::RemoveTargetEvidence::NotRemovedByOperation
                ) {
                    // Unknown or future evidence cannot authorize another unlink.
                    // Preserve responsibility for explicit recovery adjudication.
                    device
                        .unresolved_removals
                        .lock()
                        .map_err(|_| error(DeviceOperation::ReclaimRetirements))?
                        .push(object);
                }
                evidence == pi_async_fs::RemoveTargetEvidence::RemovedByOperation
                    || (evidence == pi_async_fs::RemoveTargetEvidence::NotRemovedByOperation
                        && source
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                        && matches!(device.namespace.try_exists(&path).await, Ok(false)))
            }
        };
        if completed {
            // Drop cache records for this stable object's retired lifecycle.
            device
                .mappings
                .lock()
                .await
                .retain(|cached, _| match object {
                    RetiredObject::LargeBlock { id } => cached.large_id() != Some(id),
                    RetiredObject::Segment { level, version } => {
                        cached.level() != level || cached.segment_version() != Some(version)
                    }
                });
            let mut queue = device
                .retirements
                .lock()
                .map_err(|_| error(DeviceOperation::ReclaimRetirements))?;
            if let Some(index) = queue.iter().position(|queued| *queued == object) {
                queue.remove(index);
            }
            report.completed += 1;
        } else {
            report.retryable += 1;
        }
    }
    Ok(report)
}

pub(super) fn admit_delete(
    device: &FileBlockDevice,
    object: RetiredObject,
) -> Result<bool, DeviceError> {
    let mut gate = device
        .retirement_gate
        .lock()
        .map_err(|_| error(DeviceOperation::ReclaimRetirements))?;
    if device
        .unresolved_removals
        .lock()
        .map_err(|_| error(DeviceOperation::ReclaimRetirements))?
        .contains(&object)
    {
        return Ok(false);
    }
    let mut states = device
        .snapshot_states
        .lock()
        .map_err(|_| error(DeviceOperation::ReclaimRetirements))?;
    states.retain(|state| state.strong_count() != 0);
    if states.iter().any(|state| {
        state
            .upgrade()
            .is_some_and(|state| state_references(&state, object))
    }) {
        return Ok(false);
    }
    if !gate.contains(&object) {
        gate.push(object);
    }
    Ok(true)
}

pub(super) fn state_references(state: &BlockDeviceState, object: RetiredObject) -> bool {
    match object {
        RetiredObject::LargeBlock { id } => state.large_blocks.entries.contains_key(&id),
        RetiredObject::Segment { level, version } => state
            .levels
            .get(usize::from(level))
            .is_some_and(|versions| {
                version
                    .raw()
                    .checked_sub(versions.min_version.raw())
                    .and_then(|offset| versions.versions.get(offset as usize))
                    .is_some_and(|entry| {
                        entry
                            .as_ref()
                            .is_some_and(|data| !data.released_at.is_empty())
                    })
            }),
    }
}

fn error(operation: DeviceOperation) -> DeviceError {
    dev_err(
        operation,
        Box::new(std::io::Error::other("retirement registry poisoned")),
    )
}
