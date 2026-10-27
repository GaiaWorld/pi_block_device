use super::*;

#[test]
fn resolver_failures_are_injected_by_key_and_stage() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("resolver-failure");
    seed(&path, Arc::new(MockBackend::fixed(id(71))));
    let failing_resolver = MockResolver::with(key(), Arc::new(MockBackend::fixed(id(71)))).fail_at(
        key(),
        ResolverStage::Resolve,
        "injected resolve failure",
    );
    let (kind, operation, source) = recovery(
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
                log_config(&path),
                resolver(failing_resolver),
                codec(),
            ),
        )
        .expect_err("injected"),
    );
    assert_eq!(
        (kind, operation),
        (
            RecoveryFailureKind::ResolveDevice,
            RecoveryOperation::Resolve
        )
    );
    assert!(source.to_string().contains("injected resolve failure"));
}
#[test]
fn open_retries_only_transient_reconcile_failures_three_total_with_same_state() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("log");
    seed(&path, Arc::new(MockBackend::fixed(id(8))));
    let backend = Arc::new(MockBackend::fixed(id(8)).reconcile_failures([
        ReconcileFailureKind::RepairIo,
        ReconcileFailureKind::MediaMetadataSync,
    ]));
    futures_lite::future::block_on(
        Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
            log_config(&path),
            resolver(MockResolver::with(key(), backend.clone())),
            codec(),
        ),
    )
    .expect("third succeeds");
    assert_eq!(backend.calls.lock().expect("calls").reconcile, 3);
    let states = backend.reconcile_states();
    assert!(Arc::ptr_eq(&states[0], &states[1]) && Arc::ptr_eq(&states[1], &states[2]));
    let failing = Arc::new(
        MockBackend::fixed(id(8)).reconcile_failures([ReconcileFailureKind::ConcurrentMutation; 3]),
    );
    let (_, _, source) = recovery(
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
                log_config(&path),
                resolver(MockResolver::with(key(), failing.clone())),
                codec(),
            ),
        )
        .expect_err("three failures"),
    );
    assert_eq!(failing.calls.lock().expect("calls").reconcile, 3);
    assert!(source.source().is_some());
}
#[test]
fn open_does_not_retry_permanent_reconcile_failure() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("log");
    seed(&path, Arc::new(MockBackend::fixed(id(9))));
    let backend = Arc::new(
        MockBackend::fixed(id(9)).reconcile_failures([ReconcileFailureKind::RequiredObjectMissing]),
    );
    let (kind, operation, _) = recovery(
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
                log_config(&path),
                resolver(MockResolver::with(key(), backend.clone())),
                codec(),
            ),
        )
        .expect_err("permanent"),
    );
    assert_eq!(
        (kind, operation),
        (RecoveryFailureKind::Reconcile, RecoveryOperation::Reconcile)
    );
    assert_eq!(backend.calls.lock().expect("calls").reconcile, 1);
}

#[test]
fn open_records_failure_source_and_progress_for_each_retry_attempt() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("progress");
    seed(&path, Arc::new(MockBackend::fixed(id(70))));
    let progress = pi_block_device::ReconcileProgress {
        planned_actions: 4,
        completed_actions: 2,
        remaining_actions: 2,
        partial_progress: true,
        media_metadata_synced: false,
    };
    let backend = Arc::new(MockBackend::fixed(id(70)).reconcile_failure_queue([
        (
            ReconcileFailureKind::RepairIo,
            progress,
            Some("first".to_owned()),
        ),
        (
            ReconcileFailureKind::MediaMetadataSync,
            progress,
            Some("second".to_owned()),
        ),
    ]));
    futures_lite::future::block_on(
        Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
            log_config(&path),
            resolver(MockResolver::with(key(), backend.clone())),
            codec(),
        ),
    )
    .expect("retry succeeds");
    let calls = backend.calls.lock().expect("calls");
    assert_eq!(calls.reconcile_attempts.len(), 3);
    assert_eq!(calls.reconcile_attempts[0].source.as_deref(), Some("first"));
    assert_eq!(
        calls.reconcile_attempts[1].source.as_deref(),
        Some("second")
    );
    assert_eq!(calls.reconcile_attempts[0].progress, progress);
    assert!(calls.reconcile_attempts[2].source.is_none());
}

#[test]
fn create_registers_three_devices_and_open_resolves_them_in_order() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("three");
    let devices = [id(40), id(41), id(42)].map(|value| Arc::new(MockBackend::fixed(value)));
    let initial = devices
        .iter()
        .enumerate()
        .map(|(index, device)| InitialDevice {
            label: DeviceLabel::new(format!("d{index}")).expect("label"),
            device: device.clone(),
        })
        .collect();
    futures_lite::future::block_on(
        Superblock::<TestState, TestCodec, MockBackend, MockResolver>::create(
            log_config(&path),
            initial,
            resolver(MockResolver::default()),
            codec(),
            TestState(0),
        ),
    )
    .expect("create");
    let mut mock_resolver = MockResolver::default();
    let calls = mock_resolver.calls.clone();
    for (index, device) in devices.into_iter().enumerate() {
        mock_resolver
            .devices
            .insert(DeviceKey::new((index + 1) as u32).expect("key"), device);
    }
    futures_lite::future::block_on(
        Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
            log_config(&path),
            Arc::new(mock_resolver),
            codec(),
        ),
    )
    .expect("open");
    assert_eq!(
        *calls.lock().expect("calls"),
        (1..=3)
            .map(|value| DeviceKey::new(value).expect("key"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn open_key2_failure_stops_before_key3_resolution() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("key2");
    let devices = [id(50), id(51), id(52)].map(|value| Arc::new(MockBackend::fixed(value)));
    let initial = devices
        .iter()
        .enumerate()
        .map(|(index, device)| InitialDevice {
            label: DeviceLabel::new(format!("d{index}")).expect("label"),
            device: device.clone(),
        })
        .collect();
    futures_lite::future::block_on(
        Superblock::<TestState, TestCodec, MockBackend, MockResolver>::create(
            log_config(&path),
            initial,
            resolver(MockResolver::default()),
            codec(),
            TestState(0),
        ),
    )
    .expect("create");
    let mock_resolver = MockResolver::with(DeviceKey::new(1).expect("key"), devices[0].clone());
    let calls = mock_resolver.calls.clone();
    let error =
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
                log_config(&path),
                Arc::new(mock_resolver),
                codec(),
            ),
        )
        .expect_err("key2");
    let OpenError::RecoveryFailed { kind, context } = error else {
        panic!("whole open must fail")
    };
    assert_eq!(kind, RecoveryFailureKind::ResolveDevice);
    assert_eq!(context.device_key, Some(DeviceKey::new(2).expect("key")));
    assert_eq!(context.operation, RecoveryOperation::Resolve);
    let source = context
        .source
        .downcast_ref::<pi_superblock::ResolveError>()
        .expect("resolver source");
    assert_eq!(source.device_key, DeviceKey::new(2).expect("key"));
    assert_eq!(source.source.to_string(), "unknown device key");
    // 全部设备解析与身份校验成功前，不得对任何介质执行恢复副作用。
    assert_eq!(devices[0].calls.lock().expect("calls").reconcile, 0);
    assert_eq!(devices[1].calls.lock().expect("calls").reconcile, 0);
    assert_eq!(devices[2].calls.lock().expect("calls").reconcile, 0);
    assert_eq!(
        *calls.lock().expect("calls"),
        vec![
            DeviceKey::new(1).expect("key"),
            DeviceKey::new(2).expect("key")
        ]
    );
}
