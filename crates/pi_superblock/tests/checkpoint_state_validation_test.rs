use rpds::RedBlackTreeMapSync;

use bytes::Bytes;
use pi_block_device::{BlockDeviceId, BlockDeviceState, RecordSeq};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, BindingName,
    DeviceLabel, DeviceRegistration, DeviceSlot, DeviceTable, SuperblockError, encode_checkpoint,
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
fn registry_and_checkpoint_reject_invalid_block_device_state() {
    let label = DeviceLabel::new("primary".to_owned()).expect("valid label");
    let registration = DeviceRegistration {
        id: BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
        label,
    };
    let mut invalid_state = BlockDeviceState::initial().expect("initial state");
    invalid_state.levels[0].versions = rpds::VectorSync::new_sync();
    assert!(DeviceTable::new(vec![DeviceSlot::new(registration.clone(), invalid_state)]).is_err());
    // Invalid state cannot pass the public constructor; exercise the same rejection on wire.
    let devices = DeviceTable::new(vec![DeviceSlot::new(
        registration,
        BlockDeviceState::initial().unwrap(),
    )])
    .expect("valid device table");
    let roots = RedBlackTreeMapSync::new_sync();
    let bindings = RedBlackTreeMapSync::<BindingName, pi_block_device::BlockRef>::new_sync();

    let encoded = encode_checkpoint(
        RecordSeq::new(1).expect("valid sequence"),
        &devices,
        &roots,
        &bindings,
        &TestState(42),
        &TestCodec,
    )
    .unwrap();
    let mut bytes = encoded.to_vec();
    // count/key/id/label length/primary = 33 bytes, then min_version and version_count.
    bytes[37..41].copy_from_slice(&0_u32.to_le_bytes());
    let result = pi_superblock::decode_checkpoint::<TestState, TestCodec>(
        RecordSeq::new(1).unwrap(),
        Bytes::from(bytes),
        &TestCodec,
    );

    assert!(matches!(result, Err(SuperblockError::InvalidBlockRef)));
}
