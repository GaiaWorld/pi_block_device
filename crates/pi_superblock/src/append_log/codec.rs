use bytes::Bytes;
use pi_append_log::{BlockEncoder, DecodedBlock, DefaultBlockCodec};
use pi_block_device::RecordSeq;

use crate::format::header::{HEADER_LEN, HeaderDecodeError, decode_record_header_detailed};
use crate::{RecordHeader, RecordKind, SuperblockError, record_seq_from_block_seq};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedSuperblockRecord {
    pub record_seq: RecordSeq,
    pub header: RecordHeader,
    pub body: Bytes,
}

pub fn decode_append_log_block(
    block: DecodedBlock<'_>,
) -> Result<DecodedSuperblockRecord, SuperblockError> {
    decode_append_log_block_detailed(block).map_err(Into::into)
}

pub(crate) fn decode_append_log_block_detailed(
    block: DecodedBlock<'_>,
) -> Result<DecodedSuperblockRecord, HeaderDecodeError> {
    if block.flags() != 0 {
        return Err(HeaderDecodeError::InvalidState);
    }
    let record_seq = record_seq_from_block_seq(block.block_seq())
        .map_err(|_| HeaderDecodeError::InvalidState)?;
    let payload = block.payload();
    let header_bytes = payload
        .get(..HEADER_LEN)
        .ok_or(HeaderDecodeError::InvalidState)?;
    let header = decode_record_header_detailed(header_bytes)?;
    Ok(DecodedSuperblockRecord {
        record_seq,
        header,
        body: Bytes::copy_from_slice(&payload[HEADER_LEN..]),
    })
}

pub fn encode_append_log_block(
    codec: &DefaultBlockCodec,
    record_seq: RecordSeq,
    kind: RecordKind,
    body: &[u8],
) -> Result<Vec<u8>, SuperblockError> {
    if record_seq.is_null() {
        return Err(SuperblockError::RecordSeqExhausted);
    }
    let header = crate::encode_record_header(kind)?;
    let payload_len = header
        .len()
        .checked_add(body.len())
        .ok_or(SuperblockError::RecordTooLarge)?;
    if payload_len > codec.max_payload_len() {
        return Err(SuperblockError::RecordTooLarge);
    }
    let mut payload = Vec::with_capacity(payload_len);
    payload.extend_from_slice(&header);
    payload.extend_from_slice(body);
    codec
        .encode(record_seq.raw() as u64, 0, &payload)
        .map_err(|_| SuperblockError::RecordTooLarge)
}
