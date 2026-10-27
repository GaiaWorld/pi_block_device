use bytes::Bytes;
use pi_block_device::{BlockDeviceId, BlockDeviceState, DeviceKey, RecordSeq};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, DeviceLabel,
    DeviceRegistration, DeviceSlot, DeviceTable, SuperblockError, decode_checkpoint,
    encode_checkpoint,
};
use rpds::RedBlackTreeMapSync;
#[derive(Clone, Debug, Eq, PartialEq)]
struct App;
impl ApplicationState for App {
    type Delta = ();
}
struct Codec;
impl ApplicationCodec<App> for Codec {
    fn encode_state(
        &self,
        _: &App,
        out: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        out.put_slice(b"APP1")
    }
    fn decode_state(&self, bytes: Bytes) -> Result<App, ApplicationCodecError> {
        // This self-describing application codec validates its complete input, not a consumed count.
        if bytes.as_ref() != b"APP1" {
            return Err(ApplicationCodecError::new("expected complete APP1 payload"));
        }
        Ok(App)
    }
    fn encode_delta(
        &self,
        _: &(),
        _: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
    }
    fn decode_delta(&self, bytes: Bytes) -> Result<(), ApplicationCodecError> {
        if !bytes.is_empty() {
            return Err(ApplicationCodecError::new("expected empty delta"));
        }
        Ok(())
    }
}
fn seq() -> RecordSeq {
    RecordSeq::new(2).unwrap()
}
fn slot(id: u8, label: &str) -> DeviceSlot {
    DeviceSlot::new(
        DeviceRegistration {
            id: BlockDeviceId::from_bytes([id; 16]).unwrap(),
            label: DeviceLabel::new(label.into()).unwrap(),
        },
        BlockDeviceState::initial().unwrap(),
    )
}
fn golden() -> Vec<u8> {
    // Independent literal fields: count, key, ID, label, fifteen levels, cursor and map counts.
    let mut bytes = vec![1, 0, 0, 0, 1, 0, 0, 0];
    bytes.extend_from_slice(&[1; 16]);
    bytes.extend_from_slice(&[1, 0, b'a']);
    for _ in 0..15 {
        bytes.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
    bytes.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(b"APP1");
    bytes
}
fn decode(bytes: Vec<u8>) -> Result<(), SuperblockError> {
    decode_checkpoint::<App, Codec>(seq(), Bytes::from(bytes), &Codec).map(|_| ())
}
#[test]
fn literal_golden_matches_frozen_order_and_roundtrips() {
    let table = DeviceTable::new(vec![slot(1, "a")]).unwrap();
    let encoded = encode_checkpoint(
        seq(),
        &table,
        &RedBlackTreeMapSync::new_sync(),
        &RedBlackTreeMapSync::new_sync(),
        &App,
        &Codec,
    )
    .unwrap();
    assert_eq!(encoded.as_ref(), golden());
    let (devices, roots, bindings, app) = decode_checkpoint(seq(), encoded, &Codec).unwrap();
    assert_eq!(devices, table);
    assert!(roots.is_empty());
    assert!(bindings.is_empty());
    assert_eq!(app, App);
}
#[test]
fn borrowed_views_and_clone_share_registration_storage() {
    let table = DeviceTable::new(vec![slot(1, "hot"), slot(2, "Hot")]).unwrap();
    let clone = table.clone();
    assert_eq!(table.iter().count(), 2);
    assert!(!table.is_empty());
    for (index, view) in table.iter().enumerate() {
        let key = DeviceKey::new(index as u32 + 1).unwrap();
        assert!(std::ptr::eq(
            view.registration(),
            clone.get(key).unwrap().registration()
        ));
        assert!(std::ptr::eq(view.state(), table.get(key).unwrap().state()));
    }
    assert!(table.get(DeviceKey::new(u32::MAX).unwrap()).is_none());
    assert!(DeviceKey::new(0).is_err());
    assert!(BlockDeviceId::from_bytes([0; 16]).is_err());
    assert!(DeviceTable::new(vec![]).is_err());
}
#[test]
fn strict_option_window_id_and_sequence_rejection() {
    for (offset, value) in [(35, 2), (31, 0), (27, 0)] {
        let mut bytes = golden();
        bytes[offset] = value;
        assert!(matches!(
            decode(bytes),
            Err(SuperblockError::InvalidBlockRef)
        ));
    }
    let mut bytes = golden();
    bytes[8..24].fill(0);
    assert!(matches!(
        decode(bytes),
        Err(SuperblockError::InvalidBlockRef)
    ));
    assert!(
        decode_checkpoint::<App, Codec>(RecordSeq::NULL, Bytes::from(golden()), &Codec).is_err()
    );
}
#[test]
fn application_codec_rejects_its_own_trailing_bytes() {
    let mut bytes = golden();
    bytes.push(0);
    assert!(matches!(
        decode(bytes),
        Err(SuperblockError::ApplicationCodec { .. })
    ));
}
#[test]
fn bounded_count_and_exact_release_count() {
    assert!(matches!(
        decode(1_000_001_u32.to_le_bytes().to_vec()),
        Err(SuperblockError::RecordTooLarge)
    ));
    assert!(matches!(
        decode(1_000_000_u32.to_le_bytes().to_vec()),
        Err(SuperblockError::InvalidBlockRef)
    ));
    let mut bytes = golden();
    bytes[40..44].copy_from_slice(&1_u32.to_le_bytes());
    assert!(matches!(
        decode(bytes),
        Err(SuperblockError::InvalidBlockRef)
    ));
}
#[test]
fn exhausted_large_cursor_roundtrips_zero_is_rejected() {
    let mut bytes = golden();
    bytes[282..290].copy_from_slice(&((1_u64 << 60) - 1).to_le_bytes());
    let (devices, _, _, _) =
        decode_checkpoint::<App, Codec>(seq(), Bytes::from(bytes), &Codec).unwrap();
    assert_eq!(
        devices
            .get(DeviceKey::new(1).unwrap())
            .unwrap()
            .state()
            .large_blocks
            .next_id
            .raw(),
        (1_u64 << 60) - 1
    );
    let mut bytes = golden();
    bytes[282..290].fill(0);
    assert!(matches!(
        decode(bytes),
        Err(SuperblockError::InvalidBlockRef)
    ));
}

fn allocated_golden(allocated: u32, released: u32) -> Vec<u8> {
    let mut bytes = golden();
    let mut segment = vec![1, 0, 0, 0];
    segment.extend_from_slice(&allocated.to_le_bytes());
    segment.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);
    segment.extend_from_slice(&released.to_le_bytes());
    bytes.splice(36..44, segment);
    bytes
}
#[test]
fn lifecycle_exact_count_and_null_rules_use_independent_bytes() {
    assert!(decode(allocated_golden(1, 0)).is_ok());
    assert!(decode(allocated_golden(1, 2)).is_ok());
    for (allocated, released) in [(0, 0), (3, 0), (1, 1), (1, 3), (1, u32::MAX)] {
        assert!(matches!(
            decode(allocated_golden(allocated, released)),
            Err(SuperblockError::InvalidBlockRef)
        ));
    }
    let mut bytes = allocated_golden(1, 0);
    bytes[48..52].copy_from_slice(&2_u32.to_le_bytes());
    assert!(matches!(
        decode(bytes),
        Err(SuperblockError::InvalidBlockRef)
    ));
}
#[test]
fn roots_bindings_and_large_entries_reject_duplicates_or_nonascending_keys() {
    // A live ordinary block is raw=16 (version1,level0,index0), DeviceKey1.
    let block = [1, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0];
    let mut bytes = allocated_golden(1, 0);
    bytes.truncate(306);
    bytes.extend_from_slice(&[2, 0, 0, 0]);
    for _ in 0..2 {
        bytes.extend_from_slice(&block);
        bytes.extend_from_slice(&[1, 0, 0, 0]);
    }
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(b"APP1");
    assert!(matches!(
        decode(bytes),
        Err(SuperblockError::InvalidBlockRef)
    ));
    for names in [*b"ba", *b"aa"] {
        let mut bytes = allocated_golden(1, 0);
        bytes.truncate(306);
        bytes.extend_from_slice(&[1, 0, 0, 0]);
        bytes.extend_from_slice(&block);
        bytes.extend_from_slice(&[1, 0, 0, 0]);
        bytes.extend_from_slice(&[2, 0, 0, 0]);
        for name in names {
            bytes.extend_from_slice(&[1, 0, 0, 0, name]);
            bytes.extend_from_slice(&block);
        }
        bytes.extend_from_slice(b"APP1");
        assert!(matches!(
            decode(bytes),
            Err(SuperblockError::InvalidBlockRef)
        ));
    }
    for ids in [[2_u64, 1], [1, 1]] {
        let mut bytes = golden();
        bytes.truncate(282);
        bytes.extend_from_slice(&3_u64.to_le_bytes());
        bytes.extend_from_slice(&[2, 0, 0, 0]);
        for id in ids {
            bytes.extend_from_slice(&id.to_le_bytes());
            bytes.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        }
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(b"APP1");
        assert!(matches!(
            decode(bytes),
            Err(SuperblockError::InvalidBlockRef)
        ));
    }
}
