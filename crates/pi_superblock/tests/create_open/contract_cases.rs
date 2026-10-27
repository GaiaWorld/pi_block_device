use super::*;
use create_open_mock::InspectMode;
use futures_lite::future::block_on;
use pi_block_device::{BackendCapabilities, DeviceError, DeviceOperation, ReconcileError};

type TestSuperblock = Superblock<TestState, TestCodec, MockBackend, MockResolver>;

#[test]
fn inspection_error_preserves_device_operation_and_source() {
    let temp = tempdir().unwrap();
    let mut backend = MockBackend::fixed(id(80));
    backend.inspect_mode = InspectMode::Failure;
    let backend = Arc::new(backend);
    let error = block_on(create_future(
        &temp.path().join("inspection"),
        backend.clone(),
    ))
    .unwrap_err();
    let CreateError::NotCreated {
        kind,
        source: Some(source),
    } = error
    else {
        panic!("inspection failure")
    };
    assert_eq!(kind, CreateFailureKind::DeviceInspection);
    let source = source.downcast_ref::<DeviceError>().unwrap();
    assert_eq!(source.device_key, Some(key()));
    assert_eq!(source.operation, DeviceOperation::Inspect);
    assert_eq!(
        source.source.to_string(),
        "mock backend operation unavailable"
    );
    assert_eq!(backend.calls.lock().unwrap().inspect, 1);
    assert!(!temp.path().join("inspection").exists());
}

fn expected_values(
    variant: CapabilityVariant,
) -> (CapabilityField, CapabilityValue, CapabilityValue) {
    let fixed = BackendCapabilities::fixed();
    match variant {
        CapabilityVariant::Protocol => (
            CapabilityField::ProtocolVersion,
            CapabilityValue::U16(1),
            CapabilityValue::U16(2),
        ),
        CapabilityVariant::Base => (
            CapabilityField::BaseBlockSize,
            CapabilityValue::U32(fixed.base_block_size),
            CapabilityValue::U32(fixed.base_block_size + 1),
        ),
        CapabilityVariant::Level => {
            let mut actual = fixed.level_sizes;
            actual[0] += 1;
            (
                CapabilityField::LevelSizes,
                CapabilityValue::Levels(fixed.level_sizes),
                CapabilityValue::Levels(actual),
            )
        }
        CapabilityVariant::Large => (
            CapabilityField::IndependentLargeBlocks,
            CapabilityValue::Bool(true),
            CapabilityValue::Bool(false),
        ),
        CapabilityVariant::Optional => panic!("optional extra is compatible"),
    }
}

#[test]
fn create_and_open_fixed_fields_preserve_required_actual_and_source() {
    for (variant, failing_key) in [
        (CapabilityVariant::Protocol, 1),
        (CapabilityVariant::Base, 2),
        (CapabilityVariant::Level, 3),
        (CapabilityVariant::Large, 1),
    ] {
        let temp = tempdir().unwrap();
        let path = temp.path().join("log");
        let make_initial = |mismatch: bool| {
            (1..=3)
                .map(|number| InitialDevice {
                    label: DeviceLabel::new(format!("device-{number}")).unwrap(),
                    device: Arc::new(if mismatch && number == failing_key {
                        MockBackend::fixed(id(number as u8)).incompatible_variant(variant)
                    } else {
                        MockBackend::fixed(id(number as u8))
                    }),
                })
                .collect()
        };
        let error = block_on(TestSuperblock::create(
            log_config(&path),
            make_initial(true),
            resolver(MockResolver::default()),
            codec(),
            TestState(0),
        ))
        .unwrap_err();
        let CreateError::NotCreated {
            kind,
            source: Some(source),
        } = error
        else {
            panic!("capability failure")
        };
        assert_eq!(kind, CreateFailureKind::IncompatibleDeviceCapabilities);
        let expected = expected_values(variant);
        let mismatch = source.downcast_ref::<CapabilityMismatchError>().unwrap();
        assert_eq!(mismatch.device_key, DeviceKey::new(failing_key).unwrap());
        assert_eq!(
            (&mismatch.field, &mismatch.required, &mismatch.actual),
            (&expected.0, &expected.1, &expected.2)
        );
        block_on(TestSuperblock::create(
            log_config(&path),
            make_initial(false),
            resolver(MockResolver::default()),
            codec(),
            TestState(0),
        ))
        .unwrap();
        let mut mock = MockResolver::default();
        for number in 1..=3 {
            let backend = if number == failing_key {
                MockBackend::fixed(id(number as u8)).incompatible_variant(variant)
            } else {
                MockBackend::fixed(id(number as u8))
            };
            mock.devices
                .insert(DeviceKey::new(number).unwrap(), Arc::new(backend));
        }
        let error = block_on(TestSuperblock::open(
            log_config(&path),
            resolver(mock),
            codec(),
        ))
        .unwrap_err();
        let OpenError::RecoveryFailed { kind, context } = error else {
            panic!("open failure")
        };
        assert_eq!(kind, RecoveryFailureKind::IncompatibleDeviceCapabilities);
        assert_eq!(context.operation, RecoveryOperation::ValidateCapabilities);
        assert_eq!(
            context.device_key,
            Some(DeviceKey::new(failing_key).unwrap())
        );
        let mismatch = context
            .source
            .downcast_ref::<CapabilityMismatchError>()
            .unwrap();
        assert_eq!(mismatch.device_key, DeviceKey::new(failing_key).unwrap());
        assert_eq!(
            (&mismatch.field, &mismatch.required, &mismatch.actual),
            (&expected.0, &expected.1, &expected.2)
        );
    }
}

#[test]
fn optional_extra_is_accepted_by_create_and_open() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("optional");
    let backend =
        Arc::new(MockBackend::fixed(id(81)).incompatible_variant(CapabilityVariant::Optional));
    seed(&path, backend);
    let mut actual = BackendCapabilities::fixed();
    actual.optional_flags = u64::MAX;
    let backend = Arc::new(MockBackend::fixed(id(81)).with_capabilities(actual));
    let opened = block_on(TestSuperblock::open(
        log_config(&path),
        resolver(MockResolver::with(key(), backend)),
        codec(),
    ))
    .unwrap();
    assert_eq!(opened.status(), SuperblockStatus::Ready);
}

#[test]
fn all_five_reconcile_kinds_obey_retry_limit_and_preserve_final_source() {
    for (kind, attempts) in [
        (ReconcileFailureKind::RepairIo, 3),
        (ReconcileFailureKind::MediaMetadataSync, 3),
        (ReconcileFailureKind::ConcurrentMutation, 3),
        (ReconcileFailureKind::RequiredObjectMissing, 1),
        (ReconcileFailureKind::RequiredObjectCorrupt, 1),
    ] {
        let temp = tempdir().unwrap();
        let path = temp.path().join("retry");
        seed(&path, Arc::new(MockBackend::fixed(id(82))));
        let progress = pi_block_device::ReconcileProgress {
            planned_actions: 4,
            completed_actions: 1,
            remaining_actions: 3,
            partial_progress: true,
            media_metadata_synced: false,
        };
        let backend = Arc::new(MockBackend::fixed(id(82)).reconcile_failure_queue(
            (1..=4).map(|n| (kind, progress, Some(format!("attempt-{n}")))),
        ));
        let (outer, operation, source) = recovery(
            block_on(TestSuperblock::open(
                log_config(&path),
                resolver(MockResolver::with(key(), backend.clone())),
                codec(),
            ))
            .unwrap_err(),
        );
        assert_eq!(
            (outer, operation),
            (RecoveryFailureKind::Reconcile, RecoveryOperation::Reconcile)
        );
        let error = source.downcast_ref::<ReconcileError>().unwrap();
        assert_eq!(error.kind, kind);
        assert_eq!(error.progress, progress);
        assert_eq!(
            error.source.as_ref().unwrap().to_string(),
            format!("attempt-{attempts}")
        );
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.reconcile, attempts);
        assert_eq!(calls.reconcile_attempts.len(), attempts);
        for (index, attempt) in calls.reconcile_attempts.iter().enumerate() {
            assert!(Arc::ptr_eq(
                &attempt.state,
                &calls.reconcile_attempts[0].state
            ));
            assert_eq!(attempt.progress, progress);
            assert_eq!(
                attempt.source.as_deref(),
                Some(format!("attempt-{}", index + 1).as_str())
            );
        }
        drop(calls);
        if attempts == 3 {
            let backend = Arc::new(MockBackend::fixed(id(82)).reconcile_failures([kind; 2]));
            block_on(TestSuperblock::open(
                log_config(&path),
                resolver(MockResolver::with(key(), backend.clone())),
                codec(),
            ))
            .unwrap();
            assert_eq!(backend.calls.lock().unwrap().reconcile, 3);
            let states = backend.reconcile_states();
            assert!(states.iter().all(|state| Arc::ptr_eq(state, &states[0])));
        }
    }
}
