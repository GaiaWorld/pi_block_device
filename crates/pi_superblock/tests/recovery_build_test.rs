use rpds::RedBlackTreeMapSync;

use bytes::Bytes;
use futures_lite::future::block_on;
use pi_append_log::DefaultBlockCodec;
use pi_block_device::{
    AllocationRun, BlockDeviceId, BlockDeviceState, BlockId, BlockRef, DeviceKey, RecordSeq,
    SegmentVersion,
};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, BindingName,
    DeviceLabel, DeviceRegistration, DeviceSlot, DeviceTable, DurableAppendOutcome, RecordKind,
    SuperblockAppendLog, SuperblockStatus,
};
use tempfile::tempdir;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState(u32);

impl ApplicationState for TestState {
    type Delta = u32;
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
            .map_err(|_| ApplicationCodecError::new("invalid test state"))?;
        Ok(TestState(u32::from_le_bytes(raw)))
    }

    fn encode_delta(
        &self,
        delta: &u32,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        output.put_u32_le(*delta)
    }

    fn decode_delta(&self, bytes: Bytes) -> Result<u32, ApplicationCodecError> {
        let raw: [u8; 4] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| ApplicationCodecError::new("invalid test delta"))?;
        Ok(u32::from_le_bytes(raw))
    }
}

fn test_checkpoint() -> (Bytes, BlockRef, BindingName) {
    let record_seq = RecordSeq::new(1).expect("record sequence 1 must be valid");
    let device_key = DeviceKey::new(1).expect("device key 1 must be valid");
    let mut device_state = BlockDeviceState::initial().expect("initial device state");
    let active_segment = device_state.levels[0]
        .versions
        .get_mut(0)
        .and_then(Option::as_mut)
        .expect("initial active segment");
    active_segment.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: record_seq,
        len: 1,
    });
    active_segment.released_at.push_back_mut(RecordSeq::NULL);

    let live_block = BlockRef {
        device: device_key,
        block: BlockId::ordinary(
            0,
            SegmentVersion::new(1).expect("segment version 1 must be valid"),
            0,
        )
        .expect("ordinary live block must be valid"),
    };
    let devices = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: BlockDeviceId::from_bytes([1; 16]).expect("device id must be valid"),
            label: DeviceLabel::new("primary".to_owned()).expect("device label must be valid"),
        },
        device_state,
    )])
    .expect("device table must be valid");
    let roots = [(live_block, record_seq)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    let binding_name = BindingName::new(b"main".to_vec()).expect("binding name must be valid");
    let bindings = [(binding_name.clone(), live_block)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    let body = pi_superblock::encode_checkpoint(
        record_seq,
        &devices,
        &roots,
        &bindings,
        &TestState(42),
        &TestCodec,
    )
    .expect("legal checkpoint must encode");
    (body, live_block, binding_name)
}

#[test]
fn build_recovering_reconstructs_a_durable_checkpoint() {
    block_on(async {
        let temp = tempdir().expect("create isolated append-log directory");
        let path = temp.path().join("recovery-build-log");
        let codec = DefaultBlockCodec::default();
        let max_payload_len = codec.max_payload_len();
        let record_seq = RecordSeq::new(1).expect("record sequence 1 must be valid");
        let (checkpoint_body, live_block, binding_name) = test_checkpoint();

        let log = SuperblockAppendLog::open(path.clone(), max_payload_len)
            .await
            .expect("empty real file append log must open");
        assert!(
            matches!(
                log.append_durable(record_seq, RecordKind::Checkpoint, checkpoint_body)
                    .await,
                DurableAppendOutcome::Durable { record_seq: actual, .. } if actual == record_seq
            ),
            "checkpoint must be durably appended before recovery"
        );
        drop(log);

        let (log, recovered) = pi_superblock::build_recovering(path, max_payload_len, &TestCodec)
            .await
            .expect("build_recovering must return the recovered tuple");
        drop(log);

        assert_eq!(recovered.status, SuperblockStatus::Ready);
        assert_eq!(recovered.state.record_seq, record_seq);
        assert_eq!(recovered.state.application, TestState(42));
        assert_eq!(recovered.state.devices.len(), 1);
        let device_key = DeviceKey::new(1).expect("device key 1 must be valid");
        assert_eq!(
            recovered
                .state
                .devices
                .get(device_key)
                .expect("primary device must be present")
                .registration()
                .label
                .as_bytes(),
            b"primary"
        );
        assert_eq!(recovered.state.roots.size(), 1);
        assert_eq!(recovered.state.roots.get(&live_block), Some(&record_seq));
        assert_eq!(recovered.state.bindings.size(), 1);
        assert_eq!(
            recovered.state.bindings.get(&binding_name),
            Some(&live_block)
        );
    });
}
