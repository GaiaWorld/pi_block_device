//! Private candidate changes and runtime read/commit lifecycle.

use super::*;

pub(super) fn block_write(
    allocation: AllocatedBlock,
    data: Bytes,
) -> Result<BlockWrite, SuperblockError> {
    if data.len() as u64 != allocation.logical_len.get() {
        return Err(SuperblockError::InvalidBlockRef);
    }
    Ok(BlockWrite { allocation, data })
}

impl<'a, S, C, D, R> Transaction<'a, S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub(super) fn application_impl(&self) -> &S {
        &self.candidate.application
    }

    pub(super) fn get_binding_impl(&self, _name: &BindingName) -> Option<BlockRef> {
        self.candidate.bindings.get(_name).copied()
    }

    pub(super) async fn commit_impl(&mut self) -> Result<CommitResult, SuperblockError> {
        use pi_block_device::BlockDeviceWriteBatch;
        self.ensure_open()?;
        // Local ownership closes commit on error/cancellation without a transaction phase.
        let _writer = self.writer_guard.take();
        let mut batches = std::mem::take(&mut self.batches);
        let seq = self.candidate_seq()?;
        if self.allocations.iter().any(|record| !record.written) {
            return Err(SuperblockError::CommitFailed);
        }
        if !self
            .candidate
            .devices
            .iter()
            .all(|slot| slot.state().validate_at(seq))
            || self
                .candidate
                .roots
                .keys()
                .chain(self.candidate.bindings.values())
                .any(|block| !crate::state::is_live_block(&self.candidate.devices, *block, seq))
        {
            return Err(SuperblockError::InvalidBlockRef);
        }
        let body = crate::encode_delta::<S, C>(seq, &self.changes, self._codec)?;
        for (index, batch) in batches.iter_mut().enumerate() {
            if let Some(batch) = batch.take() {
                batch.prepare().await.map_err(|mut source| {
                    source.operation = pi_block_device::DeviceOperation::Prepare;
                    source.device_key = DeviceKey::new(index as u32 + 1).ok();
                    SuperblockError::Device { source }
                })?;
            }
        }
        let mut fence = AppendFence {
            status: self.status,
            armed: true,
        };
        let outcome = self
            .log
            .append_durable(seq, crate::RecordKind::Delta, body)
            .await;
        match outcome {
            crate::DurableAppendOutcome::NotAppended { .. } => {
                fence.armed = false;
                return Err(SuperblockError::CommitFailed);
            }
            crate::DurableAppendOutcome::Unknown { .. } => {
                return Err(SuperblockError::CommitStatusUnknown);
            }
            crate::DurableAppendOutcome::Durable { record_seq, .. } => {
                if record_seq != seq {
                    self.status
                        .update(crate::SuperblockStatus::RecoveryRequired);
                }
            }
        }
        // Durable success cannot be reported as a failed business commit.
        Arc::make_mut(&mut self.candidate).record_seq = seq;
        if let Ok(_publication) = self.publication_access.lock() {
            self.published.store(Arc::clone(&self.candidate));
            if seq.next().is_err() {
                self.status
                    .update(crate::SuperblockStatus::ReadOnlyExhausted);
            }
        } else {
            self.status
                .update(crate::SuperblockStatus::RecoveryRequired);
            // metadata已durable，但旧发布视图仍持有存活对象，不能登记其退休。
            // 重新open依据durable候选扫描真实介质并补偿，不在此处抢先回收。
            fence.armed = false;
            return Ok(CommitResult {
                record_seq: seq,
                status: self.status.load(),
            });
        }
        for (device, retired) in self.devices.iter().zip(release::retirements_between(
            &self.published_at_begin.devices,
            &self.candidate.devices,
        )) {
            if !retired.is_empty() && device.enqueue_retirements(retired).is_err() {
                self.status
                    .update(crate::SuperblockStatus::RecoveryRequired);
            }
        }
        fence.armed = false;
        Ok(CommitResult {
            record_seq: seq,
            status: self.status.load(),
        })
    }

    pub(super) fn apply_application_impl(
        &mut self,
        delta: S::Delta,
    ) -> Result<(), SuperblockError> {
        self.ensure_open()?;
        let context = ApplicationApplyContext::new(
            &self.candidate.devices,
            &self.candidate.roots,
            &self.candidate.bindings,
            self.candidate_seq()?,
            self.changes.len() as u32,
        );
        let next = self
            .candidate
            .application
            .apply(&delta, &context)
            .map_err(|source| SuperblockError::ApplicationApply { source })?;
        Arc::make_mut(&mut self.candidate).application = next;
        self.changes.push(TransactionChange::Application(delta));
        Ok(())
    }

    pub(super) fn add_root_impl(&mut self, block: BlockRef) -> Result<(), SuperblockError> {
        self.system(SystemChange::AddRoot { block })
    }

    pub(super) fn remove_root_impl(&mut self, block: BlockRef) -> Result<(), SuperblockError> {
        self.system(SystemChange::RemoveRoot { block })
    }

    pub(super) fn bind_impl(
        &mut self,
        name: BindingName,
        block: BlockRef,
    ) -> Result<(), SuperblockError> {
        self.system(SystemChange::AddBinding { name, block })
    }

    pub(super) fn unbind_impl(&mut self, name: &BindingName) -> Result<(), SuperblockError> {
        self.system(SystemChange::RemoveBinding { name: name.clone() })
    }

    pub(super) fn system(&mut self, change: SystemChange) -> Result<(), SuperblockError> {
        self.ensure_open()?;
        let record_seq = self.candidate_seq()?;
        let mut next = Arc::clone(&self.candidate);
        crate::change::apply_system_change(Arc::make_mut(&mut next), &change, record_seq)?;
        self.candidate = next;
        self.changes.push(TransactionChange::System(change));
        Ok(())
    }

    pub(super) fn device_index(&self, device: DeviceKey) -> Result<usize, SuperblockError> {
        let index = device
            .raw()
            .checked_sub(1)
            .ok_or(SuperblockError::InvalidBlockRef)? as usize;
        (index < self.devices.len())
            .then_some(index)
            .ok_or(SuperblockError::InvalidBlockRef)
    }

    pub(super) fn candidate_seq(&self) -> Result<pi_block_device::RecordSeq, SuperblockError> {
        self.candidate
            .record_seq
            .next()
            .map_err(|_| SuperblockError::RecordSeqExhausted)
    }

    pub(super) fn ensure_open(&self) -> Result<(), SuperblockError> {
        if self.status.load() == crate::SuperblockStatus::RecoveryRequired {
            return Err(SuperblockError::RecoveryRequired);
        }
        if self.writer_guard.is_none() {
            return Err(SuperblockError::TransactionClosed);
        }
        if self.status.load() == crate::SuperblockStatus::ReadOnlyExhausted {
            return Err(SuperblockError::ReadOnlyExhausted);
        }
        Ok(())
    }

    pub(super) fn cleanup(&mut self) {
        // Only natural field RAII follows this boundary; no unlink, await or spawn.
        // Async operations locally own batches/guard; metadata uncertainty has a local fence.
    }
}

// Only metadata append uncertainty is armed; no persistent transaction stage.
struct AppendFence<'a> {
    status: &'a crate::state::AtomicStatus,
    armed: bool,
}
impl Drop for AppendFence<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.status
                .update(crate::SuperblockStatus::RecoveryRequired);
        }
    }
}

pub(super) async fn read<'a, S: ApplicationState, D: pi_block_device::BlockDeviceBackend>(
    _baseline: &'a SuperblockState<S>,
    _candidate: &'a SuperblockState<S>,
    _devices: &'a [Arc<D>],
    _status: &'a crate::state::AtomicStatus,
    open: bool,
    _block: BlockRef,
) -> Result<D::ReadBuf, SuperblockError> {
    if _status.load() == crate::SuperblockStatus::RecoveryRequired {
        return Err(SuperblockError::RecoveryRequired);
    }
    if !open {
        return Err(SuperblockError::TransactionClosed);
    }
    if !crate::state::is_live_block(&_baseline.devices, _block, _baseline.record_seq)
        || !crate::state::is_live_block(&_candidate.devices, _block, _baseline.record_seq)
    {
        return Err(SuperblockError::InvalidBlockRef);
    }
    let index =
        usize::try_from(_block.device.raw() - 1).map_err(|_| SuperblockError::InvalidBlockRef)?;
    let device = _devices
        .get(index)
        .ok_or(SuperblockError::InvalidBlockRef)?;
    let state = _baseline
        .devices
        .get(_block.device)
        .ok_or(SuperblockError::InvalidBlockRef)?;
    // A lease protects this read only, not the whole transaction baseline.
    let lease = device
        .acquire_snapshot_lease(Arc::new(state.state().clone()))
        .map_err(|mut source| {
            source.operation = pi_block_device::DeviceOperation::AcquireSnapshotLease;
            source.device_key = Some(_block.device);
            SuperblockError::Device { source }
        })?;
    device
        .read(&lease, _block.block)
        .await
        .map_err(|mut source| {
            source.operation = pi_block_device::DeviceOperation::Read;
            source.device_key = Some(_block.device);
            SuperblockError::Device { source }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_block_device::{BlockId, SegmentVersion};

    #[test]
    fn borrowed_guard_is_send_and_inline_capacity_is_not_a_device_limit() {
        fn send<T: Send>(_: &T) {}
        let mutex = async_lock::Mutex::new(());
        let guard: async_lock::MutexGuard<'_, ()> = futures_lite::future::block_on(mutex.lock());
        send(&guard);
        drop(guard);
        let mut devices = smallvec::SmallVec::<[Arc<()>; 4]>::new();
        for _ in 0..5 {
            devices.push(Arc::new(()));
        }
        assert_eq!(devices.len(), 5);
        assert!(devices.spilled());
    }

    #[test]
    fn block_write_preserves_exact_length_check_without_identity() {
        let allocation = || AllocatedBlock {
            block: BlockRef {
                device: DeviceKey::new(1).unwrap(),
                block: BlockId::ordinary(0, SegmentVersion::new(1).unwrap(), 0).unwrap(),
            },
            logical_len: NonZeroU64::new(3).unwrap(),
        };
        assert!(BlockWrite::new(allocation(), Bytes::from_static(b"abc")).is_ok());
        assert!(matches!(
            BlockWrite::new(allocation(), Bytes::from_static(b"ab")),
            Err(SuperblockError::InvalidBlockRef)
        ));
    }
}
