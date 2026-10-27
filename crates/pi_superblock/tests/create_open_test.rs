mod create_open_mock;

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use pi_append_log::AppendOptions;
use pi_block_device::{BlockDeviceId, DeviceKey, ReconcileFailureKind};
use pi_superblock::{
    ApplicationCodec, ApplicationEncoder, ApplicationState, CapabilityField,
    CapabilityMismatchError, CapabilityValue, CreateError, CreateFailureKind, DeviceLabel,
    InitialDevice, OpenError, RecoveryFailureKind, RecoveryOperation, Superblock,
    SuperblockLogConfig, SuperblockStatus,
};
use tempfile::tempdir;

use create_open_mock::{CapabilityVariant, MockBackend, MockResolver, ResolverStage};

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState(u64);
impl ApplicationState for TestState {
    type Delta = ();
}
struct TestCodec;
impl ApplicationCodec<TestState> for TestCodec {
    fn encode_state(
        &self,
        state: &TestState,
        output: &mut ApplicationEncoder,
    ) -> Result<(), pi_superblock::ApplicationCodecError> {
        output.put_u64_le(state.0)
    }
    fn decode_state(
        &self,
        bytes: bytes::Bytes,
    ) -> Result<TestState, pi_superblock::ApplicationCodecError> {
        Ok(TestState(u64::from_le_bytes(
            bytes[..8].try_into().expect("state bytes"),
        )))
    }
    fn encode_delta(
        &self,
        _: &(),
        _: &mut ApplicationEncoder,
    ) -> Result<(), pi_superblock::ApplicationCodecError> {
        Ok(())
    }
    fn decode_delta(&self, _: bytes::Bytes) -> Result<(), pi_superblock::ApplicationCodecError> {
        Ok(())
    }
}
fn assert_send_static<T: Send + 'static>(_: &T) {}
fn key() -> DeviceKey {
    DeviceKey::new(1).expect("device key")
}
fn id(value: u8) -> BlockDeviceId {
    BlockDeviceId::from_bytes([value; 16]).expect("device id")
}
fn log_config(path: &Path) -> SuperblockLogConfig {
    SuperblockLogConfig {
        path: path.to_path_buf(),
        rotate_threshold: 1024 * 1024,
        append_log_options: AppendOptions { durable: true },
    }
}
fn initial_device(device: Arc<MockBackend>) -> InitialDevice<MockBackend> {
    InitialDevice {
        label: DeviceLabel::new("data".to_owned()).expect("label"),
        device,
    }
}
fn codec() -> Arc<TestCodec> {
    Arc::new(TestCodec)
}
fn resolver(value: MockResolver) -> Arc<MockResolver> {
    Arc::new(value)
}
fn create_future(
    path: &Path,
    backend: Arc<MockBackend>,
) -> impl Future<
    Output = Result<Superblock<TestState, TestCodec, MockBackend, MockResolver>, CreateError>,
> + Send
+ 'static {
    Superblock::create(
        log_config(path),
        vec![initial_device(backend)],
        resolver(MockResolver::default()),
        codec(),
        TestState(0),
    )
}
fn seed(path: &Path, backend: Arc<MockBackend>) {
    futures_lite::future::block_on(create_future(path, backend)).expect("seed create");
}

#[test]
fn create_and_open_futures_are_send_static() {
    let temp = tempdir().expect("tempdir");
    let create = create_future(
        &temp.path().join("log"),
        Arc::new(MockBackend::fixed(id(1))),
    );
    assert_send_static(&create);
    let open = Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
        log_config(&temp.path().join("other")),
        resolver(MockResolver::default()),
        codec(),
    );
    assert_send_static(&open);
}
#[test]
fn create_writes_real_log_at_only_configured_path_without_other_backend_calls() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("superblock-log");
    let backend = Arc::new(MockBackend::fixed(id(2)));
    let calls = backend.calls.clone();
    let resolver = MockResolver::with(key(), backend.clone());
    let resolver_calls = resolver.calls.clone();
    let mut config = log_config(&path);
    config.append_log_options.durable = false;
    futures_lite::future::block_on(
        Superblock::<TestState, TestCodec, MockBackend, MockResolver>::create(
            config,
            vec![initial_device(backend)],
            Arc::new(resolver),
            codec(),
            TestState(7),
        ),
    )
    .expect("create");
    assert!(path.is_dir());
    assert!(
        std::fs::metadata(path.join("00000001"))
            .expect("segment")
            .len()
            > 0
    );
    assert_eq!(resolver_calls.lock().expect("calls").len(), 0);
    let calls = calls.lock().expect("backend calls");
    assert_eq!(calls.inspect, 1);
    assert_eq!(calls.begin_write, 0);
    assert_eq!(calls.acquire_snapshot_lease, 0);
    assert_eq!(calls.read, 0);
    assert_eq!(calls.reconcile, 0);
    assert_eq!(calls.enqueue_retirements, 0);
}
#[test]
fn create_maps_non_empty_and_capability_mismatch() {
    let temp = tempdir().expect("tempdir");
    let empty =
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::create(
                log_config(&temp.path().join("empty")),
                Vec::new(),
                resolver(MockResolver::default()),
                codec(),
                TestState(0),
            ),
        )
        .expect_err("empty");
    assert!(matches!(
        empty,
        CreateError::NotCreated {
            kind: CreateFailureKind::InvalidConfiguration,
            source: None
        }
    ));
    let duplicate_label =
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::create(
                log_config(&temp.path().join("duplicate-label")),
                vec![
                    initial_device(Arc::new(MockBackend::fixed(id(30)))),
                    InitialDevice {
                        label: DeviceLabel::new("data".to_owned()).expect("label"),
                        device: Arc::new(MockBackend::fixed(id(31))),
                    },
                ],
                resolver(MockResolver::default()),
                codec(),
                TestState(0),
            ),
        )
        .expect_err("duplicate label");
    assert!(matches!(
        duplicate_label,
        CreateError::NotCreated {
            kind: CreateFailureKind::InvalidConfiguration,
            source: None
        }
    ));
    let duplicate_id =
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::create(
                log_config(&temp.path().join("duplicate-id")),
                vec![
                    initial_device(Arc::new(MockBackend::fixed(id(32)))),
                    InitialDevice {
                        label: DeviceLabel::new("other".to_owned()).expect("label"),
                        device: Arc::new(MockBackend::fixed(id(32))),
                    },
                ],
                resolver(MockResolver::default()),
                codec(),
                TestState(0),
            ),
        )
        .expect_err("duplicate id");
    assert!(matches!(
        duplicate_id,
        CreateError::NotCreated {
            kind: CreateFailureKind::InvalidConfiguration,
            source: None
        }
    ));
    let temp = tempdir().expect("tempdir");
    let non_empty = futures_lite::future::block_on(create_future(
        &temp.path().join("a"),
        Arc::new(MockBackend::fixed(id(3)).non_empty()),
    ))
    .expect_err("non-empty");
    assert!(matches!(
        non_empty,
        CreateError::NotCreated {
            kind: CreateFailureKind::DeviceNotEmpty,
            ..
        }
    ));
    let mismatch = futures_lite::future::block_on(create_future(
        &temp.path().join("b"),
        Arc::new(MockBackend::fixed(id(4)).incompatible()),
    ))
    .expect_err("mismatch");
    let CreateError::NotCreated {
        kind: CreateFailureKind::IncompatibleDeviceCapabilities,
        source: Some(source),
    } = mismatch
    else {
        panic!("mismatch")
    };
    let mismatch = source
        .downcast_ref::<CapabilityMismatchError>()
        .expect("structured mismatch");
    assert_eq!(mismatch.device_key, key());
    assert_eq!(mismatch.field, CapabilityField::ProtocolVersion);
    assert_eq!(mismatch.required, CapabilityValue::U16(1));
    assert_eq!(mismatch.actual, CapabilityValue::U16(2));
}

#[test]
fn create_rejects_each_capability_variant() {
    for (index, variant) in [
        CapabilityVariant::Protocol,
        CapabilityVariant::Base,
        CapabilityVariant::Level,
        CapabilityVariant::Large,
    ]
    .into_iter()
    .enumerate()
    {
        let temp = tempdir().expect("tempdir");
        let error = futures_lite::future::block_on(create_future(
            &temp.path().join("variant"),
            Arc::new(MockBackend::fixed(id(index as u8 + 60)).incompatible_variant(variant)),
        ))
        .expect_err("variant");
        assert!(matches!(
            error,
            CreateError::NotCreated {
                kind: CreateFailureKind::IncompatibleDeviceCapabilities,
                ..
            }
        ));
    }
}
fn recovery(
    error: OpenError,
) -> (
    RecoveryFailureKind,
    RecoveryOperation,
    pi_block_device::BoxError,
) {
    let OpenError::RecoveryFailed { kind, context } = error else {
        panic!("recovery")
    };
    assert_eq!(context.device_key, Some(key()));
    (kind, context.operation, context.source)
}
#[test]
fn open_reconciles_and_reports_ready() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("log");
    let backend = Arc::new(MockBackend::fixed(id(5)));
    seed(&path, backend.clone());
    let opened =
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
                log_config(&path),
                resolver(MockResolver::with(key(), backend)),
                codec(),
            ),
        )
        .expect("open");
    assert_eq!(opened.status(), SuperblockStatus::Ready);
}
#[test]
fn open_maps_resolution_identity_and_capability_failures() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("log");
    seed(&path, Arc::new(MockBackend::fixed(id(6))));
    let (kind, operation, _) = recovery(
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
                log_config(&path),
                resolver(MockResolver::default()),
                codec(),
            ),
        )
        .expect_err("resolve"),
    );
    assert_eq!(
        (kind, operation),
        (
            RecoveryFailureKind::ResolveDevice,
            RecoveryOperation::Resolve
        )
    );
    let (kind, operation, _) = recovery(
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
                log_config(&path),
                resolver(MockResolver::with(
                    key(),
                    Arc::new(MockBackend::fixed(id(7))),
                )),
                codec(),
            ),
        )
        .expect_err("identity"),
    );
    assert_eq!(
        (kind, operation),
        (
            RecoveryFailureKind::DeviceIdentityMismatch,
            RecoveryOperation::ValidateIdentity
        )
    );
    let (kind, operation, source) = recovery(
        futures_lite::future::block_on(
            Superblock::<TestState, TestCodec, MockBackend, MockResolver>::open(
                log_config(&path),
                resolver(MockResolver::with(
                    key(),
                    Arc::new(MockBackend::fixed(id(6)).incompatible()),
                )),
                codec(),
            ),
        )
        .expect_err("capability"),
    );
    assert_eq!(
        (kind, operation),
        (
            RecoveryFailureKind::IncompatibleDeviceCapabilities,
            RecoveryOperation::ValidateCapabilities
        )
    );
    assert!(source.downcast_ref::<CapabilityMismatchError>().is_some());
}

#[path = "create_open/contract_cases.rs"]
mod contract_cases;
#[path = "create_open/recovery_cases.rs"]
mod recovery_cases;
