//! 公共错误类型。

use std::fmt;

/// 块标识构造或解码失败。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidBlockId;
/// 块写入位置不满足协议约束。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidBlockWritePosition;
/// 块写入分片不满足协议约束。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidBlockWriteChunk;
/// 记录序号已到达不可回绕的耗尽状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordSeqExhausted;
macro_rules! unit_error {
    ($t:ty, $m:literal) => {
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str($m)
            }
        }
        impl std::error::Error for $t {}
    };
}
unit_error!(InvalidBlockId, "invalid block device protocol value");
unit_error!(InvalidBlockWritePosition, "invalid block write position");
unit_error!(InvalidBlockWriteChunk, "invalid block write chunk");
unit_error!(RecordSeqExhausted, "record sequence exhausted");
