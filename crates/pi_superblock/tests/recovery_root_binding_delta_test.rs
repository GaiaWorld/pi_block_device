use rpds::RedBlackTreeMapSync;
use std::io;
use std::path::PathBuf;

use bytes::Bytes;
use futures_lite::future::block_on;
use pi_append_log::DefaultBlockCodec;
use pi_block_device::{
    AllocationRun, BlockDeviceId, BlockDeviceState, BlockId, BlockRef, DeviceKey, RecordSeq,
    SegmentVersion,
};
use pi_superblock::{
    ApplicationApplyContext, ApplicationApplyError, ApplicationCodec, ApplicationCodecError,
    ApplicationEncoder, ApplicationState, BindingName, DeviceLabel, DeviceRegistration, DeviceSlot,
    DeviceTable, DurableAppendOutcome, RecordKind, SuperblockAppendLog, SystemChange,
    TransactionChange, encode_checkpoint, encode_delta,
};
use tempfile::tempdir;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState(u32);

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestDelta {
    add: u32,
    expected_published_at: u32,
}

impl ApplicationState for TestState {
    type Delta = TestDelta;

    fn apply(
        &self,
        delta: &Self::Delta,
        context: &ApplicationApplyContext<'_>,
    ) -> Result<Self, ApplicationApplyError> {
        let expected_seq = RecordSeq::new(2).expect("sequence 2 is valid");
        let expected_root = live_block(DeviceKey::new(1).expect("device key 1 is valid"));
        let roots: Vec<_> = context.roots().collect();
        let valid = context.record_seq == expected_seq
            && context.change_index == 1
            && roots.len() == 1
            && roots[0].block == expected_root
            && roots[0].published_at == seq(delta.expected_published_at)
            && context.bindings().next().is_none()
            && context.device_state(expected_root.device).is_some()
            && context.device_state(DeviceKey::new(2).unwrap()).is_none();

        if !valid {
            return Err(ApplicationApplyError {
                change_index: context.change_index,
                source: Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "application must observe seq=2 index=1, published root, and no binding; got seq={:?} index={} roots={roots:?} bindings={}",
                        context.record_seq,
                        context.change_index,
                        context.bindings().count(),
                    ),
                )),
            });
        }

        Ok(Self(self.0 + delta.add))
    }
}

struct TestCodec;

impl ApplicationCodec<TestState> for TestCodec {
    fn encode_state(
        &self,
        state: &TestState,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u32_le(state.0)
    }

    fn decode_state(&self, bytes: Bytes) -> Result<TestState, ApplicationCodecError> {
        Ok(TestState(decode_u32(bytes, "state")?))
    }

    fn encode_delta(
        &self,
        delta: &TestDelta,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u32_le(delta.add)?;
        output.put_u32_le(delta.expected_published_at)
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<TestDelta, ApplicationCodecError> {
        let raw: [u8; 8] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("test delta must be eight bytes"))?;
        Ok(TestDelta {
            add: u32::from_le_bytes(raw[..4].try_into().expect("four-byte add slice")),
            expected_published_at: u32::from_le_bytes(
                raw[4..].try_into().expect("four-byte sequence slice"),
            ),
        })
    }
}

fn decode_u32(bytes: Bytes, kind: &str) -> Result<u32, ApplicationCodecError> {
    let raw: [u8; 4] = bytes
        .as_ref()
        .try_into()
        .map_err(|_| ApplicationCodecError::new(format!("test {kind} must be four bytes")))?;
    Ok(u32::from_le_bytes(raw))
}

fn seq(value: u32) -> RecordSeq {
    RecordSeq::new(value).expect("fixture sequence is valid")
}

fn main_binding() -> BindingName {
    BindingName::new(b"main".to_vec()).expect("main binding is valid")
}

fn live_block(device: DeviceKey) -> BlockRef {
    BlockRef {
        device,
        block: BlockId::ordinary(
            0,
            SegmentVersion::new(1).expect("segment version 1 is valid"),
            0,
        )
        .expect("ordinary live block is valid"),
    }
}

fn checkpoint_parts(
    include_root_and_binding: bool,
) -> (
    DeviceTable,
    RedBlackTreeMapSync<BlockRef, RecordSeq>,
    RedBlackTreeMapSync<BindingName, BlockRef>,
    BlockRef,
) {
    let checkpoint_seq = seq(1);
    let device_key = DeviceKey::new(1).expect("device key 1 is valid");
    let mut device_state = BlockDeviceState::initial().expect("initial device state is valid");
    let active_segment = device_state.levels[0]
        .versions
        .get_mut(0)
        .and_then(Option::as_mut)
        .expect("initial active segment exists");
    active_segment.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: checkpoint_seq,
        len: 1,
    });
    active_segment.released_at.push_back_mut(RecordSeq::NULL);

    let block = live_block(device_key);
    let devices = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: BlockDeviceId::from_bytes([1; 16]).expect("device id is valid"),
            label: DeviceLabel::new("primary".to_owned()).expect("device label is valid"),
        },
        device_state,
    )])
    .expect("device table is valid");

    if include_root_and_binding {
        (
            devices,
            [(block, checkpoint_seq)]
                .into_iter()
                .collect::<RedBlackTreeMapSync<_, _>>(),
            [(main_binding(), block)]
                .into_iter()
                .collect::<RedBlackTreeMapSync<_, _>>(),
            block,
        )
    } else {
        (
            devices,
            RedBlackTreeMapSync::new_sync(),
            RedBlackTreeMapSync::new_sync(),
            block,
        )
    }
}

async fn write_real_log(path: PathBuf, checkpoint: Bytes, delta: Bytes, max_payload_len: usize) {
    let log = SuperblockAppendLog::open(path, max_payload_len)
        .await
        .expect("empty real append log must open");
    assert!(matches!(
        log.append_durable(seq(1), RecordKind::Checkpoint, checkpoint)
            .await,
        DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq(1)
    ));
    assert!(matches!(
        log.append_durable(seq(2), RecordKind::Delta, delta).await,
        DurableAppendOutcome::Durable { record_seq, .. } if record_seq == seq(2)
    ));
}

#[test]
fn recovery_replays_add_root_application_add_binding_in_wire_order() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("add-root-binding-delta-log");
        let max_payload_len = DefaultBlockCodec::default().max_payload_len();
        let (devices, roots, bindings, live) = checkpoint_parts(false);
        let checkpoint = encode_checkpoint(
            seq(1),
            &devices,
            &roots,
            &bindings,
            &TestState(10),
            &TestCodec,
        )
        .expect("checkpoint must encode");
        let changes = vec![
            TransactionChange::System(SystemChange::AddRoot { block: live }),
            TransactionChange::Application(TestDelta {
                add: 5,
                expected_published_at: 2,
            }),
            TransactionChange::System(SystemChange::AddBinding {
                name: main_binding(),
                block: live,
            }),
        ];
        let delta = encode_delta::<TestState, TestCodec>(seq(2), &changes, &TestCodec)
            .expect("mixed delta must encode");
        write_real_log(path.clone(), checkpoint, delta, max_payload_len).await;

        let (log, recovered) = pi_superblock::build_recovering(path, max_payload_len, &TestCodec)
            .await
            .expect("recovery must replay system and application changes in wire order");
        drop(log);

        assert_eq!(recovered.state.record_seq, seq(2));
        assert_eq!(recovered.state.application, TestState(15));
        assert_eq!(recovered.state.roots.get(&live), Some(&seq(2)));
        assert_eq!(recovered.state.bindings.get(&main_binding()), Some(&live));
    });
}

#[test]
fn recovery_replays_remove_binding_application_remove_root_in_wire_order() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("remove-root-binding-delta-log");
        let max_payload_len = DefaultBlockCodec::default().max_payload_len();
        let (devices, roots, bindings, live) = checkpoint_parts(true);
        let checkpoint = encode_checkpoint(
            seq(1),
            &devices,
            &roots,
            &bindings,
            &TestState(10),
            &TestCodec,
        )
        .expect("checkpoint must encode");
        let changes = vec![
            TransactionChange::System(SystemChange::RemoveBinding {
                name: main_binding(),
            }),
            TransactionChange::Application(TestDelta {
                add: 5,
                expected_published_at: 1,
            }),
            TransactionChange::System(SystemChange::RemoveRoot { block: live }),
        ];
        let delta = encode_delta::<TestState, TestCodec>(seq(2), &changes, &TestCodec)
            .expect("mixed removal delta must encode");
        write_real_log(path.clone(), checkpoint, delta, max_payload_len).await;

        let (log, recovered) = pi_superblock::build_recovering(path, max_payload_len, &TestCodec)
            .await
            .expect("recovery must replay removals and application change in wire order");
        drop(log);

        assert_eq!(recovered.state.record_seq, seq(2));
        assert_eq!(recovered.state.application, TestState(15));
        assert!(!recovered.state.roots.contains_key(&live));
        assert!(!recovered.state.bindings.contains_key(&main_binding()));
    });
}
