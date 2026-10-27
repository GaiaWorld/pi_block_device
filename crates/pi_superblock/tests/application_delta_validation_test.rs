use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};

struct AllocationObserver;

// The pinned nightly misreports this lint for these already-const TLS initializers.
mod allocation_state {
    #![allow(clippy::missing_const_for_thread_local)]

    use super::Cell;
    thread_local! {
        pub(super) static OBSERVE_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        pub(super) static LARGEST_OBSERVED_ALLOCATION: Cell<usize> = const { Cell::new(0) };
    }
}

use allocation_state::{LARGEST_OBSERVED_ALLOCATION, OBSERVE_ALLOCATIONS};

#[global_allocator]
static ALLOCATOR: AllocationObserver = AllocationObserver;

// SAFETY: Layouts, pointers, and sizes are passed unchanged to System; observation only reads
// allocation sizes and never dereferences pointers. Callers and System retain GlobalAlloc's
// allocation, deallocation, and reallocation contract.
unsafe impl GlobalAlloc for AllocationObserver {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if record_allocation(layout.size()) {
            std::ptr::null_mut()
        } else {
            // SAFETY: The caller supplies a valid Layout, which is forwarded unchanged to
            // System; the observation above only reads its size and does not inspect memory.
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The caller guarantees ptr and Layout satisfy deallocation requirements; both
        // are forwarded unchanged to System, and the observer never dereferences ptr.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if record_allocation(new_size) {
            std::ptr::null_mut()
        } else {
            // SAFETY: The caller guarantees ptr, Layout, and new_size satisfy reallocation
            // requirements. They are forwarded unchanged to System, while observation only
            // reads new_size and never dereferences ptr.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
}

fn record_allocation(size: usize) -> bool {
    OBSERVE_ALLOCATIONS.with(|observing| {
        if !observing.get() {
            return false;
        }
        LARGEST_OBSERVED_ALLOCATION.with(|largest| largest.set(largest.get().max(size)));
        size > 1024
    })
}

struct AllocationObservationGuard {
    previous_observing: bool,
    previous_largest: usize,
}

impl AllocationObservationGuard {
    fn start() -> Self {
        OBSERVE_ALLOCATIONS.with(|observing| {
            LARGEST_OBSERVED_ALLOCATION.with(|largest| Self {
                previous_observing: observing.replace(true),
                previous_largest: largest.replace(0),
            })
        })
    }
}

impl Drop for AllocationObservationGuard {
    fn drop(&mut self) {
        LARGEST_OBSERVED_ALLOCATION.with(|largest| largest.set(self.previous_largest));
        OBSERVE_ALLOCATIONS.with(|observing| observing.set(self.previous_observing));
    }
}

fn observe_largest_allocation<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    let guard = AllocationObservationGuard::start();
    let result = operation();
    let largest = LARGEST_OBSERVED_ALLOCATION.with(Cell::get);
    drop(guard);
    (result, largest)
}

use bytes::Bytes;
use pi_block_device::{DeviceKey, LargeBlockId, RecordSeq};
use pi_superblock::{
    ApplicationCodec, ApplicationCodecError, ApplicationEncoder, ApplicationState, SuperblockError,
    SystemChange, TransactionChange, decode_delta, encode_delta,
};

const DELTA_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestState;

impl ApplicationState for TestState {
    type Delta = ();
}

#[derive(Clone, Copy)]
enum EncodeBehavior {
    Empty,
    OversizedSingleWrite,
}

struct ValidationCodec {
    encode_behavior: EncodeBehavior,
    encode_calls: AtomicUsize,
    decode_error: Option<&'static str>,
}

impl ValidationCodec {
    fn empty() -> Self {
        Self {
            encode_behavior: EncodeBehavior::Empty,
            encode_calls: AtomicUsize::new(0),
            decode_error: None,
        }
    }

    fn oversized() -> Self {
        Self {
            encode_behavior: EncodeBehavior::OversizedSingleWrite,
            encode_calls: AtomicUsize::new(0),
            decode_error: None,
        }
    }

    fn failing_decode(message: &'static str) -> Self {
        Self {
            encode_behavior: EncodeBehavior::Empty,
            encode_calls: AtomicUsize::new(0),
            decode_error: Some(message),
        }
    }
}

impl ApplicationCodec<TestState> for ValidationCodec {
    fn encode_state(
        &self,
        _state: &TestState,
        _output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        Ok(())
    }

    fn decode_state(&self, _bytes: Bytes) -> Result<TestState, ApplicationCodecError> {
        Ok(TestState)
    }

    fn encode_delta(
        &self,
        _delta: &(),
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError> {
        self.encode_calls.fetch_add(1, Ordering::SeqCst);
        match self.encode_behavior {
            EncodeBehavior::Empty => Ok(()),
            EncodeBehavior::OversizedSingleWrite => {
                let payload = vec![0_u8; DELTA_LIMIT + 1];
                output.put_slice(&payload)
            }
        }
    }

    fn decode_delta(&self, _bytes: Bytes) -> Result<(), ApplicationCodecError> {
        match self.decode_error {
            Some(message) => Err(ApplicationCodecError::new(message)),
            None => Ok(()),
        }
    }
}

fn valid_seq() -> RecordSeq {
    RecordSeq::new(1).expect("sequence 1 is valid")
}

fn encoded_change(
    transaction_tag: u8,
    change_tag: u8,
    flags: u16,
    payload_len: u32,
    payload: &[u8],
) -> Bytes {
    let mut bytes = Vec::with_capacity(12 + payload.len());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.push(transaction_tag);
    bytes.push(change_tag);
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(payload);
    Bytes::from(bytes)
}

#[test]
fn oversized_single_codec_write_maps_to_record_too_large() {
    let codec = ValidationCodec::oversized();
    let changes = [TransactionChange::Application(())];

    let result = encode_delta::<TestState, ValidationCodec>(valid_seq(), &changes, &codec);

    assert!(
        matches!(result, Err(SuperblockError::RecordTooLarge)),
        "a single 16 MiB + 1 codec write must map to RecordTooLarge, got {result:?}"
    );
    assert_eq!(codec.encode_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn null_record_seq_with_empty_changes_maps_to_record_seq_exhausted() {
    let codec = ValidationCodec::empty();
    let changes: [TransactionChange<()>; 0] = [];

    let result = encode_delta::<TestState, ValidationCodec>(RecordSeq::NULL, &changes, &codec);

    assert!(
        matches!(result, Err(SuperblockError::RecordSeqExhausted)),
        "NULL record sequence must map to RecordSeqExhausted, got {result:?}"
    );
}

#[test]
fn decode_rejects_unknown_transaction_tag() {
    let result = decode_delta::<TestState, ValidationCodec>(
        encoded_change(3, 0, 0, 0, &[]),
        &ValidationCodec::empty(),
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_nonzero_application_change_tag() {
    let result = decode_delta::<TestState, ValidationCodec>(
        encoded_change(2, 1, 0, 0, &[]),
        &ValidationCodec::empty(),
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_nonzero_flags() {
    let result = decode_delta::<TestState, ValidationCodec>(
        encoded_change(2, 0, 1, 0, &[]),
        &ValidationCodec::empty(),
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_payload_len_over_limit_without_payload() {
    let result = decode_delta::<TestState, ValidationCodec>(
        encoded_change(2, 0, 0, (DELTA_LIMIT + 1) as u32, &[]),
        &ValidationCodec::empty(),
    );

    assert!(matches!(result, Err(SuperblockError::RecordTooLarge)));
}

#[test]
fn decode_rejects_payload_len_larger_than_remaining_bytes() {
    let result = decode_delta::<TestState, ValidationCodec>(
        encoded_change(2, 0, 0, 2, &[0]),
        &ValidationCodec::empty(),
    );

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_trailing_byte() {
    let mut bytes = encoded_change(2, 0, 0, 0, &[]).to_vec();
    bytes.push(0xff);

    let result =
        decode_delta::<TestState, ValidationCodec>(Bytes::from(bytes), &ValidationCodec::empty());

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_codec_error_keeps_application_codec_class_and_source_text() {
    const SOURCE_TEXT: &str = "characterization decode failure";
    let codec = ValidationCodec::failing_decode(SOURCE_TEXT);

    let error = decode_delta::<TestState, ValidationCodec>(encoded_change(2, 0, 0, 0, &[]), &codec)
        .expect_err("codec decode failure must be returned");

    assert!(matches!(error, SuperblockError::ApplicationCodec { .. }));
    assert_eq!(
        error.source().map(ToString::to_string).as_deref(),
        Some(SOURCE_TEXT)
    );
}

#[test]
fn invalid_large_system_change_is_rejected_before_codec_encode() {
    let codec = ValidationCodec::empty();
    let changes = [TransactionChange::System(
        SystemChange::AllocateLargeBlock {
            device: DeviceKey::new(1).expect("device key 1 is valid"),
            id: LargeBlockId::new(1).expect("large block id 1 is valid"),
            // AllocateLargeBlock (tag 20) is supported, but logical_len 0 is invalid.
            logical_len: 0,
        },
    )];

    let result = encode_delta::<TestState, ValidationCodec>(valid_seq(), &changes, &codec);

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
    assert_eq!(codec.encode_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn invalid_large_system_change_after_application_is_rejected_before_any_codec_encode() {
    let codec = ValidationCodec::empty();
    let changes = [
        TransactionChange::Application(()),
        TransactionChange::System(SystemChange::AllocateLargeBlock {
            device: DeviceKey::new(1).expect("device key 1 is valid"),
            id: LargeBlockId::new(1).expect("large block id 1 is valid"),
            // AllocateLargeBlock (tag 20) is supported, but logical_len 0 is invalid.
            logical_len: 0,
        }),
    ];

    let result = encode_delta::<TestState, ValidationCodec>(valid_seq(), &changes, &codec);

    assert!(matches!(result, Err(SuperblockError::InvalidState)));
    assert_eq!(codec.encode_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn decode_rejects_count_only_wire_before_reserving_for_declared_changes() {
    const COUNT: u32 = 1_000_000;
    const INPUT_LEN: usize = size_of::<u32>();

    let bytes = Bytes::copy_from_slice(&COUNT.to_le_bytes());
    let (result, largest_allocation) = observe_largest_allocation(|| {
        decode_delta::<TestState, ValidationCodec>(bytes, &ValidationCodec::empty())
    });

    assert!(
        largest_allocation <= INPUT_LEN,
        "truncated count-only wire must be rejected before reserving from untrusted count; \
         observed a {largest_allocation}-byte allocation"
    );
    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}

#[test]
fn decode_rejects_short_item_envelope_before_reserving_for_declared_changes() {
    const COUNT: u32 = 1_000_000;
    const MIN_ITEM_ENVELOPE: usize = 8;

    let mut wire = COUNT.to_le_bytes().to_vec();
    wire.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0]);
    assert!(wire.len() - size_of::<u32>() < MIN_ITEM_ENVELOPE);
    let bytes = Bytes::from(wire);

    let (result, largest_allocation) = observe_largest_allocation(|| {
        decode_delta::<TestState, ValidationCodec>(bytes, &ValidationCodec::empty())
    });

    assert!(
        largest_allocation <= MIN_ITEM_ENVELOPE,
        "wire shorter than count * 8 must be rejected before reserving from untrusted count; \
         observed a {largest_allocation}-byte allocation"
    );
    assert!(matches!(result, Err(SuperblockError::InvalidState)));
}
