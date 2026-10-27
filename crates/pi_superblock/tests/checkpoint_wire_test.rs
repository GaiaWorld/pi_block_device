use rpds::RedBlackTreeMapSync;

use bytes::Bytes;
use pi_block_device::{
    AllocationRun, BlockDeviceState, BlockId, BlockRef, DeviceKey, RecordSeq, SegmentVersion,
};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, BindingName,
    DeviceLabel, DeviceRegistration, DeviceSlot, DeviceTable, decode_checkpoint, encode_checkpoint,
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
fn checkpoint_roundtrip_preserves_static_registry_and_state() {
    let key = DeviceKey::new(1).expect("valid key");
    let label = DeviceLabel::new("primary".to_owned()).expect("valid label");
    let devices = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: pi_block_device::BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
            label,
        },
        BlockDeviceState::initial().expect("initial state"),
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
    .expect("encode checkpoint");
    let decoded = decode_checkpoint::<TestState, TestCodec>(
        RecordSeq::new(1).expect("valid sequence"),
        encoded,
        &TestCodec,
    )
    .expect("decode checkpoint");
    assert_eq!(decoded.0.len(), 1);
    let slot = decoded.0.get(key).expect("decoded slot");
    assert_eq!(slot.registration().label.as_bytes(), b"primary");
    assert_eq!(decoded.3, TestState(42));
}

fn table_with_one_live_block() -> (DeviceTable, BlockRef) {
    let mut state = BlockDeviceState::initial().expect("initial state");
    let allocated_at = RecordSeq::new(1).expect("valid sequence");
    let segment = state.levels[0]
        .versions
        .get_mut(0)
        .and_then(Option::as_mut)
        .expect("active segment");
    segment.allocation_runs.push_back_mut(AllocationRun {
        allocated_at,
        len: 1,
    });
    segment.released_at.push_back_mut(RecordSeq::NULL);
    let device = DeviceKey::new(1).expect("valid key");
    let block = BlockRef {
        device,
        block: BlockId::ordinary(0, SegmentVersion::new(1).expect("valid version"), 0)
            .expect("valid block"),
    };
    let table = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: pi_block_device::BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
            label: DeviceLabel::new("primary".to_owned()).expect("valid label"),
        },
        state,
    )])
    .expect("valid table");
    (table, block)
}

#[test]
fn checkpoint_rejects_unknown_or_released_references_and_future_publication() {
    let sequence = RecordSeq::new(2).expect("valid sequence");
    let (devices, live_block) = table_with_one_live_block();
    let unknown_block = BlockRef {
        device: DeviceKey::new(2).expect("valid key"),
        block: live_block.block,
    };
    let mut roots = [(unknown_block, sequence)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    let bindings = RedBlackTreeMapSync::new_sync();
    assert!(
        encode_checkpoint(
            sequence,
            &devices,
            &roots,
            &bindings,
            &TestState(1),
            &TestCodec
        )
        .is_err()
    );

    roots = [(
        live_block,
        RecordSeq::new(3).expect("valid future sequence"),
    )]
    .into_iter()
    .collect::<RedBlackTreeMapSync<_, _>>();
    assert!(
        encode_checkpoint(
            sequence,
            &devices,
            &roots,
            &bindings,
            &TestState(1),
            &TestCodec
        )
        .is_err()
    );

    roots = [(live_block, sequence)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    let encoded = encode_checkpoint(
        sequence,
        &devices,
        &roots,
        &bindings,
        &TestState(1),
        &TestCodec,
    )
    .expect("live root is valid");
    let mut damaged = encoded.to_vec();
    let published_at_offset = damaged.len() - 4 - 4 - 4;
    damaged[published_at_offset..published_at_offset + 4].copy_from_slice(&3_u32.to_le_bytes());
    let decoded =
        decode_checkpoint::<TestState, TestCodec>(sequence, Bytes::from(damaged), &TestCodec);
    assert!(decoded.is_err());
}

#[test]
fn checkpoint_rejects_null_publication_and_future_allocation() {
    let sequence = RecordSeq::new(2).expect("valid sequence");
    let (devices, live_block) = table_with_one_live_block();
    let null_root = [(live_block, RecordSeq::NULL)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    assert!(
        encode_checkpoint(
            sequence,
            &devices,
            &null_root,
            &RedBlackTreeMapSync::new_sync(),
            &TestState(1),
            &TestCodec,
        )
        .is_err()
    );

    let mut future_state = BlockDeviceState::initial().expect("initial state");
    let segment = future_state.levels[0]
        .versions
        .get_mut(0)
        .and_then(Option::as_mut)
        .expect("active segment");
    segment.allocation_runs.push_back_mut(AllocationRun {
        allocated_at: RecordSeq::new(3).expect("future sequence"),
        len: 1,
    });
    segment.released_at.push_back_mut(RecordSeq::NULL);
    let future_devices = DeviceTable::new(vec![DeviceSlot::new(
        DeviceRegistration {
            id: pi_block_device::BlockDeviceId::from_bytes([1; 16]).expect("valid id"),
            label: DeviceLabel::new("future".to_owned()).expect("valid label"),
        },
        future_state,
    )])
    .expect("valid table structure");
    let future_root = [(live_block, sequence)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();
    assert!(
        encode_checkpoint(
            sequence,
            &future_devices,
            &future_root,
            &RedBlackTreeMapSync::new_sync(),
            &TestState(1),
            &TestCodec,
        )
        .is_err()
    );
}

#[test]
fn checkpoint_decoder_rejects_future_allocation_without_references() {
    let sequence = RecordSeq::new(2).expect("valid sequence");
    let (devices, _) = table_with_one_live_block();
    let encoded = encode_checkpoint(
        sequence,
        &devices,
        &RedBlackTreeMapSync::new_sync(),
        &RedBlackTreeMapSync::new_sync(),
        &TestState(1),
        &TestCodec,
    )
    .expect("encode checkpoint");
    let mut damaged = encoded.to_vec();
    let allocated_at = 1_u32.to_le_bytes();
    let future = 3_u32.to_le_bytes();
    let position = damaged
        .windows(allocated_at.len())
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == allocated_at).then_some(index))
        .nth(5)
        .expect("first level allocation sequence offset");
    damaged[position..position + future.len()].copy_from_slice(&future);

    let result =
        decode_checkpoint::<TestState, TestCodec>(sequence, Bytes::from(damaged), &TestCodec);
    assert!(matches!(
        result,
        Err(pi_superblock::SuperblockError::InvalidBlockRef)
    ));
}

#[test]
fn checkpoint_rejects_binding_target_that_is_not_a_root() {
    let sequence = RecordSeq::new(2).expect("valid sequence");
    let (devices, live_block) = table_with_one_live_block();
    let name = BindingName::new(b"main".to_vec()).expect("valid binding name");
    let bindings = [(name, live_block)]
        .into_iter()
        .collect::<RedBlackTreeMapSync<_, _>>();

    let result = encode_checkpoint(
        sequence,
        &devices,
        &RedBlackTreeMapSync::new_sync(),
        &bindings,
        &TestState(1),
        &TestCodec,
    );

    assert!(matches!(
        result,
        Err(pi_superblock::SuperblockError::InvalidBlockRef)
    ));
}
