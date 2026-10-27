//! Application state contracts and bounded application values.

mod implementation;

use rpds::RedBlackTreeMapSync;

use bytes::{Bytes, BytesMut};
use pi_block_device::{BlockDeviceState, BlockRef, DeviceKey, RecordSeq};

use crate::{
    ApplicationApplyError, ApplicationCodecError, ApplicationCodecOperation, DeviceTable,
    SuperblockError,
};

pub(crate) const STATE_LIMIT: usize = 64 * 1024 * 1024;
const DELTA_LIMIT: usize = 16 * 1024 * 1024;
const MAX_NAME_BYTES: usize = 1_048_576;

/// One published root and the record that published it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootVersion {
    pub block: BlockRef,
    pub published_at: RecordSeq,
}

/// Read-only system state visible while applying an application delta.
pub struct ApplicationApplyContext<'a> {
    device_table: &'a DeviceTable,
    roots: &'a RedBlackTreeMapSync<BlockRef, RecordSeq>,
    bindings: &'a RedBlackTreeMapSync<BindingName, BlockRef>,
    pub record_seq: RecordSeq,
    pub change_index: u32,
}

impl<'a> ApplicationApplyContext<'a> {
    #[allow(dead_code)]
    pub(crate) fn new(
        device_table: &'a DeviceTable,
        roots: &'a RedBlackTreeMapSync<BlockRef, RecordSeq>,
        bindings: &'a RedBlackTreeMapSync<BindingName, BlockRef>,
        record_seq: RecordSeq,
        change_index: u32,
    ) -> Self {
        Self {
            device_table,
            roots,
            bindings,
            record_seq,
            change_index,
        }
    }

    pub fn device_table(&self) -> &DeviceTable {
        self.device_table_impl()
    }

    pub fn device_state(&self, key: DeviceKey) -> Option<&BlockDeviceState> {
        self.device_state_impl(key)
    }

    pub fn roots(&self) -> impl Iterator<Item = RootVersion> + '_ {
        self.roots_impl()
    }

    pub fn bindings(&self) -> impl Iterator<Item = (&BindingName, BlockRef)> + '_ {
        self.bindings_impl()
    }
}

/// Application state that can be cloned into a transaction candidate.
/// Applications must ensure Clone is cheap (for example, via structural sharing).
/// Persistent system collections do not make application-owned state cheap to clone.
pub trait ApplicationState: Clone + Send + Sync + 'static {
    /// Application delta type.
    type Delta: Send + Sync + 'static;

    /// Applies one delta to an immutable state snapshot.
    fn apply(
        &self,
        _delta: &Self::Delta,
        context: &ApplicationApplyContext<'_>,
    ) -> Result<Self, ApplicationApplyError> {
        implementation::default_apply(context)
    }
}

/// Synchronous application state codec.
pub trait ApplicationCodec<S: ApplicationState>: Send + Sync + 'static {
    /// Encodes state into a bounded output.
    fn encode_state(
        &self,
        state: &S,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError>;
    /// Decodes state from owned bytes.
    fn decode_state(&self, bytes: Bytes) -> Result<S, ApplicationCodecError>;
    /// Encodes one application delta into a bounded output.
    fn encode_delta(
        &self,
        delta: &S::Delta,
        output: &mut ApplicationEncoder,
    ) -> Result<(), ApplicationCodecError>;
    /// Decodes one application delta from owned bytes.
    fn decode_delta(&self, bytes: Bytes) -> Result<S::Delta, ApplicationCodecError>;
}

/// Bounded application encoder.
pub struct ApplicationEncoder {
    buffer: BytesMut,
    hard_limit: usize,
}

impl ApplicationEncoder {
    /// Creates a state encoder with the protocol state limit.
    pub fn for_state() -> Self {
        Self::with_limit(STATE_LIMIT)
    }

    /// Creates a delta encoder with the protocol delta limit.
    pub fn for_delta() -> Self {
        Self::with_limit(DELTA_LIMIT)
    }

    pub(crate) fn with_limit(hard_limit: usize) -> Self {
        Self {
            buffer: BytesMut::new(),
            hard_limit,
        }
    }

    /// Returns remaining capacity before the hard limit.
    pub fn remaining(&self) -> usize {
        self.remaining_impl()
    }

    /// Appends bytes after checked capacity validation.
    pub fn put_slice(&mut self, bytes: &[u8]) -> Result<(), ApplicationCodecError> {
        self.put_slice_impl(bytes)
    }

    /// Appends one byte.
    pub fn put_u8(&mut self, value: u8) -> Result<(), ApplicationCodecError> {
        self.put_u8_impl(value)
    }

    /// Appends a little-endian u16.
    pub fn put_u16_le(&mut self, value: u16) -> Result<(), ApplicationCodecError> {
        self.put_u16_le_impl(value)
    }

    /// Appends a little-endian u32.
    pub fn put_u32_le(&mut self, value: u32) -> Result<(), ApplicationCodecError> {
        self.put_u32_le_impl(value)
    }

    /// Appends a little-endian u64.
    pub fn put_u64_le(&mut self, value: u64) -> Result<(), ApplicationCodecError> {
        self.put_u64_le_impl(value)
    }

    /// Freezes the owned encoded bytes.
    pub fn freeze(self) -> Bytes {
        self.freeze_impl()
    }
}

/// Validated UTF-8 device label.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeviceLabel(Box<str>);

impl DeviceLabel {
    /// Creates a label with 1..=255 UTF-8 bytes.
    pub fn new(value: String) -> Result<Self, crate::CreateError> {
        Self::new_impl(value)
    }

    /// Borrows the original UTF-8 bytes.
    pub fn as_bytes(&self) -> &[u8] {
        self.as_bytes_impl()
    }
}

/// Validated UTF-8 binding name.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BindingName(Box<str>);

impl BindingName {
    /// Creates a non-empty binding name within the protocol limit.
    pub fn new(bytes: Vec<u8>) -> Result<Self, SuperblockError> {
        Self::new_impl(bytes)
    }

    /// Borrows the original UTF-8 bytes.
    pub fn as_bytes(&self) -> &[u8] {
        self.as_bytes_impl()
    }
}
