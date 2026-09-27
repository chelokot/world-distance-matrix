use anyhow::{ensure, Result};
use rayon::prelude::*;

use crate::weight::UNREACHABLE_VALUE;

pub const MAGIC: [u8; 4] = *b"DMX1";
pub const HEADER_LEN: usize = 16;
pub const BINARY_CONTENT_TYPE: &str = "application/vnd.distance-matrix.v1";

pub fn binary_header(rows: u32, cols: u32) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4..8].copy_from_slice(&UNREACHABLE_VALUE.to_le_bytes());
    header[8..12].copy_from_slice(&rows.to_le_bytes());
    header[12..16].copy_from_slice(&cols.to_le_bytes());
    header
}

pub fn binary_len(rows: usize, cols: usize) -> usize {
    HEADER_LEN + rows * cols * 2 * size_of::<u32>()
}

#[derive(Debug, PartialEq, Eq)]
pub struct DecodedMatrix {
    pub rows: usize,
    pub cols: usize,
    pub distances: Vec<u32>,
    pub durations: Vec<u32>,
}

impl DecodedMatrix {
    pub fn distance(&self, row: usize, col: usize) -> Option<u32> {
        Some(self.distances[row * self.cols + col]).filter(|&v| v != UNREACHABLE_VALUE)
    }

    pub fn duration(&self, row: usize, col: usize) -> Option<u32> {
        Some(self.durations[row * self.cols + col]).filter(|&v| v != UNREACHABLE_VALUE)
    }
}

fn words(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b))
}

pub fn decode_binary(body: &[u8]) -> Result<DecodedMatrix> {
    ensure!(body.len() >= HEADER_LEN && body[..4] == MAGIC, "not a distance matrix body");
    let header: Vec<u32> = words(&body[4..HEADER_LEN]).collect();
    ensure!(header[0] == UNREACHABLE_VALUE, "unexpected unreachable marker");
    let (rows, cols) = (header[1] as usize, header[2] as usize);
    ensure!(body.len() == binary_len(rows, cols), "body length does not match a {rows}x{cols} matrix");
    let mut distances = Vec::with_capacity(rows * cols);
    let mut durations = Vec::with_capacity(rows * cols);
    let row_bytes = 8 * cols;
    for row in 0..rows {
        let start = HEADER_LEN + row * row_bytes;
        distances.extend(words(&body[start..start + row_bytes / 2]));
        durations.extend(words(&body[start + row_bytes / 2..start + row_bytes]));
    }
    Ok(DecodedMatrix { rows, cols, distances, durations })
}

fn push_json_array(out: &mut Vec<u8>, values: &[u32]) {
    let mut buffer = itoa::Buffer::new();
    out.push(b'[');
    for (index, &value) in values.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        if value == UNREACHABLE_VALUE {
            out.extend_from_slice(b"null");
        } else {
            out.extend_from_slice(buffer.format(value).as_bytes());
        }
    }
    out.push(b']');
}

pub fn encode_json(interleaved_rows: &[u32], rows: usize, cols: usize) -> Vec<u8> {
    assert_eq!(interleaved_rows.len(), rows * cols * 2);
    let encoded: Vec<[Vec<u8>; 2]> = (0..rows)
        .into_par_iter()
        .map(|row| {
            let values = &interleaved_rows[row * 2 * cols..(row + 1) * 2 * cols];
            let (distances, durations) = values.split_at(cols);
            let mut encoded = [Vec::with_capacity(cols * 7), Vec::with_capacity(cols * 6)];
            push_json_array(&mut encoded[0], distances);
            push_json_array(&mut encoded[1], durations);
            encoded
        })
        .collect();
    let total: usize = encoded.iter().map(|[d, t]| d.len() + t.len() + 2).sum();
    let mut out = Vec::with_capacity(total + 32);
    for (key, which) in [(&b"{\"distances\":["[..], 0), (&b"],\"times\":["[..], 1)] {
        out.extend_from_slice(key);
        for (index, row) in encoded.iter().enumerate() {
            if index > 0 {
                out.push(b',');
            }
            out.extend_from_slice(&row[which]);
        }
    }
    out.extend_from_slice(b"]}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_matches_the_specified_shape() {
        let rows = [0, 18201, 0, 1319, 18204, 0, 1343, 0];
        let json = encode_json(&rows, 2, 2);
        let parsed: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed, serde_json::json!({"distances": [[0, 18201], [18204, 0]], "times": [[0, 1319], [1343, 0]]}));
    }

    #[test]
    fn json_uses_null_for_unreachable() {
        let json = encode_json(&[0, UNREACHABLE_VALUE, 0, UNREACHABLE_VALUE], 1, 2);
        assert_eq!(json, br#"{"distances":[[0,null]],"times":[[0,null]]}"#);
    }

    #[test]
    fn binary_round_trips() {
        let mut body = binary_header(2, 3).to_vec();
        let rows: [u32; 12] = [1, 2, 3, 10, 20, 30, 4, UNREACHABLE_VALUE, 6, 40, UNREACHABLE_VALUE, 60];
        body.extend(rows.iter().flat_map(|v| v.to_le_bytes()));
        let decoded = decode_binary(&body).unwrap();
        assert_eq!(decoded.distances, vec![1, 2, 3, 4, UNREACHABLE_VALUE, 6]);
        assert_eq!(decoded.durations, vec![10, 20, 30, 40, UNREACHABLE_VALUE, 60]);
        assert_eq!(decoded.distance(1, 1), None);
        assert_eq!(decoded.duration(1, 2), Some(60));
    }

    #[test]
    fn binary_rejects_truncated_bodies() {
        let mut body = binary_header(2, 2).to_vec();
        body.extend([0u8; 12]);
        assert!(decode_binary(&body).is_err());
    }
}
