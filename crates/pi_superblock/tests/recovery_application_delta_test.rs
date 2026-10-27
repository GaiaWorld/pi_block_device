use rpds::RedBlackTreeMapSync;
use std::io;
use std::sync::{Arc, Mutex};

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
    DeviceTable, DurableAppendOutcome, RecordKind, SuperblockAppendLog, TransactionChange,
    encode_checkpoint, encode_delta,
};
use tempfile::tempdir;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState(u32);

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestDelta {
    add: u32,
}

impl ApplicationState for TestState {
    type Delta = TestDelta;

    fn apply(
        &self,
        delta: &Self::Delta,
        context: &ApplicationApplyContext<'_>,
    ) -> Result<Self, ApplicationApplyError> {
        let checkpoint_seq = RecordSeq::new(1).expect("checkpoint sequence is valid");
        let delta_seq = RecordSeq::new(2).expect("delta sequence is valid");
        let device_key = DeviceKey::new(1).expect("device key is valid");
        let checkpoint_root = live_block(device_key);
        let context_is_valid = context.record_seq == delta_seq
            && context.change_index == 0
            && context.device_table().len() == 1
            && context.device_state(device_key).is_some()
            && context
                .roots()
                .any(|root| root.block == checkpoint_root && root.published_at == checkpoint_seq)
            && context
                .bindings()
                .any(|(name, block)| name.as_bytes() == b"main" && block == checkpoint_root);

        if !context_is_valid {
            return Err(ApplicationApplyError {
                change_index: context.change_index,
                source: Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "unexpected apply context: record_seq={:?}, change_index={}, devices={}, roots={}, bindings={}",
                        context.record_seq,
                        context.change_index,
                        context.device_table().len(),
                        context.roots().count(),
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
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("test state must be four bytes"))?;
        Ok(TestState(u32::from_le_bytes(raw)))
    }

    fn encode_delta(
        &self,
        delta: &TestDelta,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u32_le(delta.add)
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<TestDelta, ApplicationCodecError> {
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("test delta must be four bytes"))?;
        Ok(TestDelta {
            add: u32::from_le_bytes(raw),
        })
    }
}

fn live_block(device: DeviceKey) -> BlockRef {
    BlockRef {
        device,
        block: BlockId::ordinary(
            0,
            SegmentVersion::new(1).expect("segment version is valid"),
            0,
        )
        .expect("live block is valid"),
    }
}

fn checkpoint_state() -> (
    DeviceTable,
    RedBlackTreeMapSync<BlockRef, RecordSeq>,
    RedBlackTreeMapSync<BindingName, BlockRef>,
    BlockRef,
    BindingName,
) {
    let checkpoint_seq = RecordSeq::new(1).expect("checkpoint sequence is valid");
    let device_key = DeviceKey::new(1).expect("device key is valid");
    let mut device_state = BlockDeviceState::initial().expect("initial device state");
    let active_segment = device_state.levels[0]
        .versions
        .get_mut(0)
        .and_then(Option::as_mut)
        .expect("initial active segment");
    active_segment.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: checkpoint_seq,
        len: 1,
    });
    active_segment.released_at.push_back_mut(RecordSeq::NULL);

    let root = live_block(device_key);
    let devices = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: BlockDeviceId::from_bytes([1; 16]).expect("device id is valid"),
            label: DeviceLabel::new("primary".to_owned()).expect("device label is valid"),
        },
        device_state,
    )])
    .expect("device table is valid");
    let roots = [(root, checkpoint_seq)].into_iter().collect();
    let main = BindingName::new(b"main".to_vec()).expect("main binding is valid");
    let bindings = [(main.clone(), root)].into_iter().collect();
    (devices, roots, bindings, root, main)
}

#[test]
fn build_recovering_applies_durable_application_delta_after_checkpoint() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("recovery-application-delta-log");
        let block_codec = DefaultBlockCodec::default();
        let max_payload_len = block_codec.max_payload_len();
        let checkpoint_seq = RecordSeq::new(1).expect("checkpoint sequence is valid");
        let delta_seq = RecordSeq::new(2).expect("delta sequence is valid");
        let (devices, roots, bindings, checkpoint_root, main_binding) = checkpoint_state();
        let checkpoint_body = encode_checkpoint(
            checkpoint_seq,
            &devices,
            &roots,
            &bindings,
            &TestState(10),
            &TestCodec,
        )
        .expect("legal checkpoint must encode");
        let changes = vec![TransactionChange::Application(TestDelta { add: 5 })];
        let delta_body = encode_delta::<TestState, TestCodec>(delta_seq, &changes, &TestCodec)
            .expect("application delta must encode");

        let log = SuperblockAppendLog::open(path.clone(), max_payload_len)
            .await
            .expect("empty real append log must open");
        assert!(matches!(
            log.append_durable(checkpoint_seq, RecordKind::Checkpoint, checkpoint_body)
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == checkpoint_seq
        ));
        assert!(matches!(
            log.append_durable(delta_seq, RecordKind::Delta, delta_body)
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == delta_seq
        ));
        drop(log);

        let (recovered_log, recovered) =
            pi_superblock::build_recovering(path, max_payload_len, &TestCodec)
                .await
                .expect("recovery must replay a durable application delta");
        drop(recovered_log);

        assert_eq!(recovered.state.record_seq, delta_seq);
        assert_eq!(recovered.state.application, TestState(15));
        assert_eq!(recovered.state.devices, devices);
        assert_eq!(recovered.state.roots, roots);
        assert_eq!(
            recovered.state.roots.get(&checkpoint_root),
            Some(&checkpoint_seq)
        );
        assert_eq!(recovered.state.bindings, bindings);
        assert_eq!(
            recovered.state.bindings.get(&main_binding),
            Some(&checkpoint_root)
        );
    });
}

#[derive(Clone, Debug)]
struct ObservedState {
    value: u32,
    calls: Arc<Mutex<Vec<(u32, u32)>>>,
}

#[derive(Clone, Debug)]
struct ObservedDelta {
    add: u32,
}

impl ApplicationState for ObservedState {
    type Delta = ObservedDelta;

    fn apply(
        &self,
        delta: &Self::Delta,
        context: &ApplicationApplyContext<'_>,
    ) -> Result<Self, ApplicationApplyError> {
        self.calls
            .lock()
            .expect("application observation mutex must not be poisoned")
            .push((context.change_index, self.value));
        Ok(Self {
            value: self.value + delta.add,
            calls: Arc::clone(&self.calls),
        })
    }
}

struct ObservedCodec {
    calls: Arc<Mutex<Vec<(u32, u32)>>>,
}

impl ApplicationCodec<ObservedState> for ObservedCodec {
    fn encode_state(
        &self,
        state: &ObservedState,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u32_le(state.value)
    }

    fn decode_state(&self, bytes: Bytes) -> Result<ObservedState, ApplicationCodecError> {
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("observed state must be four bytes"))?;
        Ok(ObservedState {
            value: u32::from_le_bytes(raw),
            calls: Arc::clone(&self.calls),
        })
    }

    fn encode_delta(
        &self,
        delta: &ObservedDelta,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u32_le(delta.add)
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<ObservedDelta, ApplicationCodecError> {
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("observed delta must be four bytes"))?;
        Ok(ObservedDelta {
            add: u32::from_le_bytes(raw),
        })
    }
}

#[test]
fn build_recovering_applies_multiple_application_changes_in_order() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("ordered-application-delta-log");
        let block_codec = DefaultBlockCodec::default();
        let max_payload_len = block_codec.max_payload_len();
        let checkpoint_seq = RecordSeq::new(1).expect("checkpoint sequence is valid");
        let delta_seq = RecordSeq::new(2).expect("delta sequence is valid");
        let (devices, roots, bindings, _, _) = checkpoint_state();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let codec = ObservedCodec {
            calls: Arc::clone(&calls),
        };
        let checkpoint_body = encode_checkpoint(
            checkpoint_seq,
            &devices,
            &roots,
            &bindings,
            &ObservedState {
                value: 10,
                calls: Arc::clone(&calls),
            },
            &codec,
        )
        .expect("legal checkpoint must encode");
        let changes = vec![
            TransactionChange::Application(ObservedDelta { add: 3 }),
            TransactionChange::Application(ObservedDelta { add: 4 }),
        ];
        let delta_body = encode_delta::<ObservedState, ObservedCodec>(delta_seq, &changes, &codec)
            .expect("two application changes must encode");

        let log = SuperblockAppendLog::open(path.clone(), max_payload_len)
            .await
            .expect("empty real append log must open");
        assert!(matches!(
            log.append_durable(checkpoint_seq, RecordKind::Checkpoint, checkpoint_body)
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == checkpoint_seq
        ));
        assert!(matches!(
            log.append_durable(delta_seq, RecordKind::Delta, delta_body)
                .await,
            DurableAppendOutcome::Durable { record_seq, .. } if record_seq == delta_seq
        ));
        drop(log);

        let (recovered_log, recovered) =
            pi_superblock::build_recovering(path, max_payload_len, &codec)
                .await
                .expect("recovery must apply both application changes");
        drop(recovered_log);

        assert_eq!(
            *calls
                .lock()
                .expect("application observation mutex must not be poisoned"),
            vec![(0, 10), (1, 13)]
        );
        assert_eq!(recovered.state.application.value, 17);
        assert_eq!(recovered.state.record_seq, delta_seq);
    });
}
