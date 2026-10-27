//! 协议标识类型。

use std::num::{NonZeroU32, NonZeroU64};

use crate::{InvalidBlockId, RecordSeqExhausted};

const LEVEL_MASK: u64 = 0x0f;
const LARGE_LEVEL: u8 = 15;
const SEGMENT_VERSION_MASK: u32 = 0x0fff_ffff;
const MAX_LARGE_BLOCK_ID: u64 = (1_u64 << 60) - 1;

/// 普通段版本号，范围为 1 至 0x0fff_ffff。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SegmentVersion(NonZeroU32);
impl SegmentVersion {
    /// 严格构造普通段版本号。
    pub fn new(raw: u32) -> Result<Self, InvalidBlockId> {
        NonZeroU32::new(raw)
            .filter(|value| value.get() <= SEGMENT_VERSION_MASK)
            .map(Self)
            .ok_or(InvalidBlockId)
    }
    /// 返回原始段版本号。
    pub fn raw(self) -> u32 {
        self.0.get()
    }
}

/// 独立大块编号，范围为 1 至 2^60-2。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LargeBlockId(NonZeroU64);
impl LargeBlockId {
    /// 严格构造独立大块编号。
    pub fn new(raw: u64) -> Result<Self, InvalidBlockId> {
        NonZeroU64::new(raw)
            .filter(|value| value.get() < MAX_LARGE_BLOCK_ID)
            .map(Self)
            .ok_or(InvalidBlockId)
    }
    /// 返回原始独立大块编号。
    pub fn raw(self) -> u64 {
        self.0.get()
    }
}

/// 独立大块分配游标，最大值是耗尽哨兵。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LargeBlockCursor(u64);
impl LargeBlockCursor {
    /// 严格构造独立大块分配游标。
    pub fn new(raw: u64) -> Result<Self, InvalidBlockId> {
        (1..=MAX_LARGE_BLOCK_ID)
            .contains(&raw)
            .then_some(Self(raw))
            .ok_or(InvalidBlockId)
    }
    /// 返回原始分配游标。
    pub fn raw(self) -> u64 {
        self.0
    }
    /// 分配一个编号并推进游标。
    #[doc(hidden)]
    pub fn next_id(self) -> Result<(LargeBlockId, Self), InvalidBlockId> {
        Ok((LargeBlockId::new(self.0)?, Self::new(self.0 + 1)?))
    }
}

/// 严格编码的块标识。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BlockId(u64);
impl BlockId {
    /// 构造普通块标识。
    pub fn ordinary(
        level: u8,
        version: SegmentVersion,
        block_index: u32,
    ) -> Result<Self, InvalidBlockId> {
        if level >= LARGE_LEVEL {
            return Err(InvalidBlockId);
        }
        Ok(Self(
            (u64::from(block_index) << 32) | (u64::from(version.raw()) << 4) | u64::from(level),
        ))
    }
    /// 构造独立大块标识。
    pub fn large(id: LargeBlockId) -> Self {
        Self((id.raw() << 4) | u64::from(LARGE_LEVEL))
    }
    /// 从原始编码严格构造块标识。
    pub fn from_raw(raw: u64) -> Result<Self, InvalidBlockId> {
        if raw == u64::MAX {
            return Err(InvalidBlockId);
        }
        if (raw & LEVEL_MASK) as u8 == LARGE_LEVEL {
            LargeBlockId::new(raw >> 4).map(|_| Self(raw))
        } else {
            SegmentVersion::new(((raw >> 4) & u64::from(SEGMENT_VERSION_MASK)) as u32)
                .map(|_| Self(raw))
        }
    }
    /// 返回原始编码。
    pub fn raw(self) -> u64 {
        self.0
    }
    /// 返回块级别。
    pub fn level(self) -> u8 {
        (self.0 & LEVEL_MASK) as u8
    }
    /// 返回普通块的段版本。
    pub fn segment_version(self) -> Option<SegmentVersion> {
        (self.level() != LARGE_LEVEL)
            .then(|| {
                SegmentVersion::new(((self.0 >> 4) & u64::from(SEGMENT_VERSION_MASK)) as u32).ok()
            })
            .flatten()
    }
    /// 返回普通块的段内索引。
    pub fn block_index(self) -> Option<u32> {
        (self.level() != LARGE_LEVEL).then_some((self.0 >> 32) as u32)
    }
    /// 返回独立大块编号。
    pub fn large_id(self) -> Option<LargeBlockId> {
        (self.level() == LARGE_LEVEL)
            .then(|| LargeBlockId::new(self.0 >> 4).ok())
            .flatten()
    }
    /// 返回普通块的段版本和索引。
    pub fn ordinary_fields(self) -> Option<(SegmentVersion, u32)> {
        self.segment_version().zip(self.block_index())
    }
    /// 返回独立大块原始编号。
    pub fn large_block_id(self) -> Option<u64> {
        self.large_id().map(LargeBlockId::raw)
    }
}

/// 记录序号；零表示生命周期 Null，最大值表示耗尽哨兵。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RecordSeq(u32);
impl RecordSeq {
    /// 生命周期字段中的 Null 序号。
    pub const NULL: Self = Self(0);
    /// 构造有效持久记录序号。
    pub fn new(raw: u32) -> Result<Self, RecordSeqExhausted> {
        (raw != 0 && raw != u32::MAX)
            .then_some(Self(raw))
            .ok_or(RecordSeqExhausted)
    }
    /// 从生命周期字段构造序号，允许 Null。
    pub fn from_lifecycle_raw(raw: u32) -> Result<Self, RecordSeqExhausted> {
        (raw != u32::MAX)
            .then_some(Self(raw))
            .ok_or(RecordSeqExhausted)
    }
    /// 返回是否为 Null 序号。
    pub fn is_null(self) -> bool {
        self == Self::NULL
    }
    /// 返回原始序号。
    pub fn raw(self) -> u32 {
        self.0
    }
    /// 返回下一个有效序号，不发生回绕。
    #[doc(hidden)]
    pub fn next(self) -> Result<Self, RecordSeqExhausted> {
        Self::new(self.0.checked_add(1).ok_or(RecordSeqExhausted)?)
    }
}

/// 非零设备注册键。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeviceKey(NonZeroU32);
impl DeviceKey {
    /// 严格构造设备键。
    pub fn new(raw: u32) -> Result<Self, InvalidBlockId> {
        NonZeroU32::new(raw).map(Self).ok_or(InvalidBlockId)
    }
    /// 返回原始设备键。
    pub fn raw(self) -> u32 {
        self.0.get()
    }
}

/// 非全零的十六字节设备身份。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BlockDeviceId([u8; 16]);
impl BlockDeviceId {
    /// 使用操作系统熵源生成设备身份。
    pub fn generate() -> Result<Self, getrandom::Error> {
        loop {
            let mut bytes = [0_u8; 16];
            getrandom::getrandom(&mut bytes)?;
            if bytes.iter().any(|byte| *byte != 0) {
                return Ok(Self(bytes));
            }
        }
    }
    /// 从字节数组严格构造设备身份。
    pub fn from_bytes(bytes: [u8; 16]) -> Result<Self, InvalidBlockId> {
        bytes
            .iter()
            .any(|byte| *byte != 0)
            .then_some(Self(bytes))
            .ok_or(InvalidBlockId)
    }
    /// 借用设备身份字节。
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// 设备键与块标识的十二字节小端引用。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BlockRef {
    /// 设备注册键。
    pub device: DeviceKey,
    /// 块标识。
    pub block: BlockId,
}
impl BlockRef {
    /// 编码为四字节设备键加八字节块标识。
    pub fn to_le_bytes(self) -> [u8; 12] {
        let mut bytes = [0_u8; 12];
        bytes[..4].copy_from_slice(&self.device.raw().to_le_bytes());
        bytes[4..].copy_from_slice(&self.block.raw().to_le_bytes());
        bytes
    }
    /// 严格解码十二字节小端引用。
    pub fn from_le_bytes(bytes: [u8; 12]) -> Result<Self, InvalidBlockId> {
        let mut device = [0_u8; 4];
        device.copy_from_slice(&bytes[..4]);
        let mut block = [0_u8; 8];
        block.copy_from_slice(&bytes[4..]);
        Ok(Self {
            device: DeviceKey::new(u32::from_le_bytes(device))?,
            block: BlockId::from_raw(u64::from_le_bytes(block))?,
        })
    }
}
