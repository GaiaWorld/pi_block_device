//! Private Superblock implementation; public contracts remain in the parent.

use super::support::*;
use super::*;
use crate::append_log::DurableAppendOutcome;
use crate::{CreateFailureKind, DeviceSlot, DeviceTable, RecoveryFailureKind, RecoveryOperation};
use pi_block_device::DeviceOperation;

impl<S, C, D, R> Superblock<S, C, D, R>
where
    S: ApplicationState,
    C: ApplicationCodec<S>,
    D: pi_block_device::BlockDeviceBackend,
    R: DeviceResolver<D>,
{
    pub(super) fn debug_impl(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Superblock")
            .field("status", &self.status())
            .finish()
    }

    pub(super) async fn create_impl(
        log: SuperblockLogConfig,
        devices: Vec<InitialDevice<D>>,
        resolver: Arc<R>,
        codec: Arc<C>,
        initial_state: S,
    ) -> Result<Self, CreateError> {
        validate_config(&log, devices.len())?;
        let mut slots = Vec::with_capacity(devices.len());
        for (index, initial) in devices.iter().enumerate() {
            let key = create_device_key(index)?;
            crate::capability::compare_capabilities(
                key,
                &pi_block_device::BackendCapabilities::fixed(),
                &initial.device.capabilities(),
            )
            .map_err(|source| CreateError::NotCreated {
                kind: CreateFailureKind::IncompatibleDeviceCapabilities,
                source: Some(source),
            })?;
            slots.push(DeviceSlot::new(
                DeviceRegistration {
                    id: initial.device.device_id(),
                    label: initial.label.clone(),
                },
                pi_block_device::BlockDeviceState::initial().map_err(|source| {
                    CreateError::NotCreated {
                        kind: CreateFailureKind::InvalidConfiguration,
                        source: Some(Box::new(source)),
                    }
                })?,
            ));
        }
        // Validate the complete registration set before inspecting any device.
        let table = DeviceTable::new(slots)?;
        for (index, initial) in devices.iter().enumerate() {
            let key = create_device_key(index)?;
            let report = initial.device.inspect().await.map_err(|mut source| {
                source.operation = DeviceOperation::Inspect;
                source.device_key = Some(key);
                CreateError::NotCreated {
                    kind: CreateFailureKind::DeviceInspection,
                    source: Some(Box::new(source)),
                }
            })?;
            if !report.empty {
                return Err(CreateError::NotCreated {
                    kind: CreateFailureKind::DeviceNotEmpty,
                    source: None,
                });
            }
        }
        let seq = pi_block_device::RecordSeq::new(1).map_err(|source| CreateError::NotCreated {
            kind: CreateFailureKind::InvalidConfiguration,
            source: Some(Box::new(source)),
        })?;
        let state = SuperblockState {
            record_seq: seq,
            devices: table,
            roots: rpds::RedBlackTreeMapSync::new_sync(),
            bindings: rpds::RedBlackTreeMapSync::new_sync(),
            application: initial_state,
        };
        // APP references have no approved visitor contract; only codec and system relations are checked.
        let body = crate::encode_checkpoint(
            seq,
            &state.devices,
            &state.roots,
            &state.bindings,
            &state.application,
            codec.as_ref(),
        )
        .map_err(map_create_codec)?;
        let adapter = SuperblockAppendLog::open_configured(
            log.path,
            crate::format::limits::CHECKPOINT_LIMIT,
            log.append_log_options,
        )
        .await
        .map_err(|source| CreateError::NotCreated {
            kind: CreateFailureKind::LogPreparation,
            source: Some(source),
        })?;
        adapter.set_rotate_threshold(log.rotate_threshold);
        match adapter
            .append_durable(seq, crate::RecordKind::Checkpoint, body)
            .await
        {
            DurableAppendOutcome::NotAppended { source } => {
                return Err(CreateError::NotCreated {
                    kind: CreateFailureKind::AppendNotStarted,
                    source: Some(source),
                });
            }
            DurableAppendOutcome::Unknown { source } => {
                return Err(CreateError::StatusUnknown { source });
            }
            DurableAppendOutcome::Durable { .. } => {}
        }
        Ok(Self {
            log: Arc::new(adapter),
            resolver,
            codec,
            devices: devices.into_iter().map(|initial| initial.device).collect(),
            published: ArcSwap::from_pointee(state),
            publication_access: Mutex::new(()),
            status: Arc::new(crate::state::AtomicStatus::new(SuperblockStatus::Ready)),
            writer_gate: async_lock::Mutex::new(()),
        })
    }

    pub(super) async fn open_impl(
        log: SuperblockLogConfig,
        resolver: Arc<R>,
        codec: Arc<C>,
    ) -> Result<Self, OpenError> {
        let (mut adapter, recovered) = crate::build_recovering(
            log.path,
            crate::format::limits::CHECKPOINT_LIMIT,
            codec.as_ref(),
        )
        .await?;
        adapter.set_append_options(log.append_log_options);
        adapter.set_rotate_threshold(log.rotate_threshold);
        let mut devices: SmallVec<[Arc<D>; INLINE_DEVICES]> = SmallVec::new();
        for (index, slot) in recovered.state.devices.iter().enumerate() {
            let key = open_device_key(index)?;
            let device = resolver
                .resolve(key, slot.registration())
                .await
                .map_err(|source| {
                    recovery_error(
                        Some(key),
                        RecoveryFailureKind::ResolveDevice,
                        RecoveryOperation::Resolve,
                        Box::new(source),
                    )
                })?;
            if device.device_id() != slot.registration().id {
                return Err(recovery_error(
                    Some(key),
                    RecoveryFailureKind::DeviceIdentityMismatch,
                    RecoveryOperation::ValidateIdentity,
                    Box::new(DeviceIdentityMismatchError),
                ));
            }
            crate::capability::compare_capabilities(
                key,
                &pi_block_device::BackendCapabilities::fixed(),
                &device.capabilities(),
            )
            .map_err(|source| {
                recovery_error(
                    Some(key),
                    RecoveryFailureKind::IncompatibleDeviceCapabilities,
                    RecoveryOperation::ValidateCapabilities,
                    source,
                )
            })?;
            devices.push(device);
        }
        // Validate all identities first; reconcile never runs against a partial resolver set.
        for (index, slot) in recovered.state.devices.iter().enumerate() {
            let key = open_device_key(index)?;
            reconcile_with_retry(&devices[index], Arc::new(slot.state().clone()))
                .await
                .map_err(|source| {
                    recovery_error(
                        Some(key),
                        RecoveryFailureKind::Reconcile,
                        RecoveryOperation::Reconcile,
                        Box::new(source),
                    )
                })?;
            // Reconcile scans actual media orphans; historical ID ranges are not objects.
        }
        Ok(Self {
            log: Arc::new(adapter),
            resolver,
            codec,
            devices,
            published: ArcSwap::from_pointee(recovered.state),
            publication_access: Mutex::new(()),
            status: Arc::new(crate::state::AtomicStatus::new(recovered.status)),
            writer_gate: async_lock::Mutex::new(()),
        })
    }

    pub(super) fn status_impl(&self) -> SuperblockStatus {
        self.status.load()
    }

    pub(super) fn open_snapshot_impl(&self) -> Result<SuperblockSnapshot<S, D>, SuperblockError> {
        let _access = self
            .publication_access
            .lock()
            .map_err(|_| SuperblockError::RecoveryRequired)?;
        if self.status() == SuperblockStatus::RecoveryRequired {
            return Err(SuperblockError::RecoveryRequired);
        }
        let state = self.published.load_full();
        let mut leases = Vec::with_capacity(self.devices.len());
        for (index, slot) in state.devices.iter().enumerate() {
            let key =
                DeviceKey::new(index as u32 + 1).map_err(|_| SuperblockError::InvalidState)?;
            leases.push(
                self.devices[index]
                    .acquire_snapshot_lease(Arc::new(slot.state().clone()))
                    .map_err(|mut source| {
                        source.operation = DeviceOperation::AcquireSnapshotLease;
                        source.device_key = Some(key);
                        SuperblockError::Device { source }
                    })?,
            );
        }
        Ok(SuperblockSnapshot::new(
            state,
            self.devices.iter().cloned().collect(),
            leases,
            Arc::clone(&self.status),
        ))
    }

    pub(super) async fn read_impl(&self, block: BlockRef) -> Result<D::ReadBuf, SuperblockError> {
        if self.status() == SuperblockStatus::RecoveryRequired {
            return Err(SuperblockError::RecoveryRequired);
        }
        let (device, lease) = {
            let _publication_access = self
                .publication_access
                .lock()
                .map_err(|_| SuperblockError::RecoveryRequired)?;
            let published = self.published.load_full();
            // Snapshot acquisition and future commit publication must use this same gate.
            if !crate::state::is_live_block(&published.devices, block, published.record_seq) {
                return Err(SuperblockError::InvalidBlockRef);
            }
            let index = usize::try_from(
                block
                    .device
                    .raw()
                    .checked_sub(1)
                    .ok_or(SuperblockError::InvalidBlockRef)?,
            )
            .map_err(|_| SuperblockError::InvalidBlockRef)?;
            let device = self
                .devices
                .get(index)
                .ok_or(SuperblockError::InvalidBlockRef)?;
            let device_state = published
                .devices
                .get(block.device)
                .ok_or(SuperblockError::InvalidBlockRef)?
                .state()
                .clone();
            let lease = device
                .acquire_snapshot_lease(Arc::new(device_state))
                .map_err(|mut source| {
                    source.operation = DeviceOperation::AcquireSnapshotLease;
                    source.device_key = Some(block.device);
                    SuperblockError::Device { source }
                })?;
            (device, lease)
        };
        device
            .read(&lease, block.block)
            .await
            .map_err(|mut source| {
                source.operation = DeviceOperation::Read;
                source.device_key = Some(block.device);
                SuperblockError::Device { source }
            })
    }

    pub(super) async fn begin_write_impl(
        &self,
    ) -> Result<Transaction<'_, S, C, D, R>, SuperblockError> {
        ensure_writable_status(self.status())?;
        let guard = self.writer_gate.lock().await;
        ensure_writable_status(self.status())?;
        let baseline = self.published.load_full();
        if baseline.record_seq.next().is_err() {
            self.status.update(SuperblockStatus::ReadOnlyExhausted);
            return Err(SuperblockError::ReadOnlyExhausted);
        }
        Ok(Transaction {
            candidate: Arc::clone(&baseline),
            published_at_begin: baseline,
            changes: Vec::new(),
            _codec: self.codec.as_ref(),
            devices: &self.devices,
            batches: (0..self.devices.len()).map(|_| None).collect(),
            _marker: std::marker::PhantomData,
            published: &self.published,
            publication_access: &self.publication_access,
            log: &self.log,
            status: &self.status,
            writer_guard: Some(guard),
            allocations: Vec::new(),
        })
    }
}
