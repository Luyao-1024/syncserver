//! Compressed archive segments: one gzip NDJSON member plus an integrity checksum.

use super::record::{ArchiveLine, StoredRecord};
use crate::{Error, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

/// A segment payload is a standard gzip stream, so operators can also read it with `zcat`.
pub(crate) struct Segment {
    pub payload: Vec<u8>,
    pub raw_bytes: u64,
    pub stored_bytes: u64,
    pub checksum: String,
}

pub(crate) fn checksum(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn compress(rows: &[StoredRecord]) -> Result<Segment> {
    let mut raw: Vec<u8> = Vec::with_capacity(rows.len() * 256);
    for row in rows {
        serde_json::to_writer(
            &mut raw,
            &ArchiveLine {
                seq: row.seq,
                record: row.record.clone(),
            },
        )
        .map_err(|_| Error::LogStorage)?;
        raw.push(b'\n');
    }
    let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&raw).map_err(|_| Error::LogStorage)?;
    let payload = encoder.finish().map_err(|_| Error::LogStorage)?;
    Ok(Segment {
        raw_bytes: raw.len() as u64,
        stored_bytes: payload.len() as u64,
        checksum: checksum(&payload),
        payload,
    })
}

/// Decompresses a segment, refusing to return rows when the payload is corrupt.
pub(crate) fn decompress(payload: &[u8], expected_checksum: &str) -> Result<Vec<StoredRecord>> {
    if checksum(payload) != expected_checksum {
        return Err(Error::LogStorage);
    }
    let mut decoded = Vec::new();
    GzDecoder::new(payload)
        .read_to_end(&mut decoded)
        .map_err(|_| Error::LogStorage)?;
    let mut rows = Vec::new();
    for line in String::from_utf8_lossy(&decoded).lines() {
        if line.trim().is_empty() {
            continue;
        }
        let ArchiveLine { seq, record } =
            serde_json::from_str(line).map_err(|_| Error::LogStorage)?;
        rows.push(StoredRecord { seq, record });
    }
    Ok(rows)
}
