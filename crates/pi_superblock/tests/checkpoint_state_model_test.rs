use rpds::RedBlackTreeMapSync;

use bytes::Bytes;
use pi_block_device::{
    AllocationRun, BlockDeviceId, BlockDeviceState, BlockId, BlockRef, DeviceKey, RecordSeq,
    SegmentVersion,
};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, BindingName,
    DeviceLabel, DeviceRegistration, DeviceSlot, DeviceTable, decode_checkpoint_state,
    encode_checkpoint,
};

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

#[test]
fn decode_checkpoint_state_preserves_complete_state_and_uses_envelope_sequence() {
    let allocated_at = RecordSeq::new(1).expect("valid allocation sequence");
    let envelope_seq = RecordSeq::new(7).expect("valid envelope sequence");
    let device_key = DeviceKey::new(1).expect("valid device key");
    let mut device_state = BlockDeviceState::initial().expect("initial device state");
    let active_segment = device_state.levels[0]
        .versions
        .get_mut(0)
        .and_then(Option::as_mut)
        .expect("active segment");
    active_segment.allocation_runs.push_back_mut(AllocationRun {
        allocated_at,
        len: 1,
    });
    active_segment.released_at.push_back_mut(RecordSeq::NULL);
    let live_block = BlockRef {
        device: device_key,
        block: BlockId::ordinary(0, SegmentVersion::new(1).expect("valid segment version"), 0)
            .expect("valid block id"),
    };
    let devices = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: BlockDeviceId::from_bytes([1; 16]).expect("valid device id"),
            label: DeviceLabel::new("primary".to_owned()).expect("valid device label"),
        },
        device_state,
    )])
    .expect("valid device table");
    let roots = [(live_block, allocated_at)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    let binding_name = BindingName::new(b"main".to_vec()).expect("valid binding name");
    let bindings = [(binding_name.clone(), live_block)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    let application = TestState(42);

    let encoded_at_one = encode_checkpoint(
        allocated_at,
        &devices,
        &roots,
        &bindings,
        &application,
        &TestCodec,
    )
    .expect("encode checkpoint at sequence 1");
    let encoded_at_seven = encode_checkpoint(
        envelope_seq,
        &devices,
        &roots,
        &bindings,
        &application,
        &TestCodec,
    )
    .expect("encode checkpoint at sequence 7");
    assert_eq!(
        encoded_at_one, encoded_at_seven,
        "record sequence must not be encoded in the checkpoint body"
    );

    let decoded =
        decode_checkpoint_state::<TestState, TestCodec>(envelope_seq, encoded_at_one, &TestCodec)
            .expect("decode checkpoint state");

    assert_eq!(decoded.record_seq, envelope_seq);
    assert_eq!(decoded.devices, devices);
    assert_eq!(decoded.devices.len(), 1);
    assert_eq!(
        decoded
            .devices
            .get(device_key)
            .expect("decoded device")
            .registration()
            .label
            .as_bytes(),
        b"primary"
    );
    assert_eq!(decoded.roots.size(), 1);
    assert_eq!(decoded.roots.get(&live_block), Some(&allocated_at));
    assert_eq!(decoded.roots, roots);
    assert_eq!(decoded.bindings.size(), 1);
    assert_eq!(decoded.bindings.get(&binding_name), Some(&live_block));
    assert_eq!(decoded.bindings, bindings);
    assert_eq!(decoded.application, application);
}
