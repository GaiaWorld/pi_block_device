//! 几何和写入值对象。

use crate::{BlockId, InvalidBlockWriteChunk, InvalidBlockWritePosition};
use bytes::Bytes;

/// 固定协议的后端能力声明。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendCapabilities {
    /// 协议版本。
    pub protocol_version: u16,
    /// 基础块大小。
    pub base_block_size: u32,
    /// 十五级普通块大小。
    pub level_sizes: [u32; 15],
    /// 是否支持独立大块。
    pub independent_large_blocks: bool,
    /// 可选能力位。
    pub optional_flags: u64,
}
impl BackendCapabilities {
    /// 返回协议唯一固定能力。
    pub const fn fixed() -> Self {
        Self {
            protocol_version: 1,
            base_block_size: 4096,
            level_sizes: [
                4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072, 196608,
                262144, 393216, 524288,
            ],
            independent_large_blocks: true,
            optional_flags: 0,
        }
    }
    /// 判断实际能力是否满足所需能力。
    pub fn is_compatible_with(&self, actual: &Self) -> bool {
        self.protocol_version == actual.protocol_version
            && self.base_block_size == actual.base_block_size
            && self.level_sizes == actual.level_sizes
            && self.independent_large_blocks == actual.independent_large_blocks
            && actual.optional_flags & self.optional_flags == self.optional_flags
    }
}

/// 已校验的块写入位置。
pub struct BlockWritePosition {
    offset: u64,
    logical_len: u64,
}
impl BlockWritePosition {
    /// 构造位置；逻辑长度非零且偏移不越界。
    pub fn new(offset: u64, logical_len: u64) -> Result<Self, InvalidBlockWritePosition> {
        (logical_len != 0 && offset <= logical_len)
            .then_some(Self {
                offset,
                logical_len,
            })
            .ok_or(InvalidBlockWritePosition)
    }
    /// 返回逻辑偏移。
    pub fn offset(&self) -> u64 {
        self.offset
    }
    /// 返回逻辑长度。
    pub fn logical_len(&self) -> u64 {
        self.logical_len
    }
}

/// 已校验的单次块写入分片。
pub struct BlockWriteChunk {
    block: BlockId,
    chunk: Bytes,
    position: BlockWritePosition,
}
impl BlockWriteChunk {
    /// 构造分片；拒绝空分片、溢出和越界。
    pub fn new(
        block: BlockId,
        chunk: Bytes,
        position: BlockWritePosition,
    ) -> Result<Self, InvalidBlockWriteChunk> {
        if chunk.is_empty() {
            return Err(InvalidBlockWriteChunk);
        }
        let end = position
            .offset
            .checked_add(chunk.len() as u64)
            .ok_or(InvalidBlockWriteChunk)?;
        if end > position.logical_len {
            return Err(InvalidBlockWriteChunk);
        }
        Ok(Self {
            block,
            chunk,
            position,
        })
    }
    /// 返回目标块。
    pub fn block(&self) -> BlockId {
        self.block
    }
    /// 借用分片内容。
    pub fn chunk(&self) -> &Bytes {
        &self.chunk
    }
    /// 返回分片偏移。
    pub fn offset(&self) -> u64 {
        self.position.offset
    }
    /// 返回逻辑块长度。
    pub fn logical_len(&self) -> u64 {
        self.position.logical_len
    }
    /// 判断分片末端是否到达逻辑块末尾。
    pub fn is_last(&self) -> bool {
        self.position.offset + self.chunk.len() as u64 == self.position.logical_len
    }
}
