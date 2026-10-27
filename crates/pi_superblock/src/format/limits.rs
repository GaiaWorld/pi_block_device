//! Shared persistent format limits.

pub(crate) const CHECKPOINT_LIMIT: usize = u32::MAX as usize;
pub(crate) const COLLECTION_LIMIT: usize = 1_000_000;
