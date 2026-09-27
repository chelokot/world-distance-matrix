use anyhow::{ensure, Result};
use rayon::prelude::*;

use crate::geo::{hilbert_index, Coord};
use crate::weight::UNREACHABLE_VALUE;
use crate::wire::DecodedMatrix;

pub const COMPACT_CONTENT_TYPE: &str = "application/vnd.distance-matrix.compact.v1";
pub const COMPACT_MAGIC: [u8; 4] = *b"DMC1";
pub const FRAME_ROWS: usize = 32;
const ZSTD_LEVEL: i32 = 3;

pub fn spatial_order(points: &[Coord]) -> Vec<u32> {
    let mut order: Vec<u32> = (0..points.len() as u32).collect();
    order.sort_by_key(|&index| (hilbert_index(points[index as usize]), index));
    order
}

pub fn compact_header(row_order: &[u32], col_order: &[u32]) -> Vec<u8> {
    let (rows, cols) = (row_order.len(), col_order.len());
    let words = [rows as u32, cols as u32, rows.div_ceil(FRAME_ROWS) as u32].into_iter().chain(row_order.iter().copied()).chain(col_order.iter().copied());
    COMPACT_MAGIC.into_iter().chain(words.flat_map(u32::to_le_bytes)).collect()
}

fn zigzag(value: i64) -> u32 {
    ((value << 1) ^ (value >> 63)) as u32
}

fn unzigzag(value: u32) -> i64 {
    (value >> 1) as i64 ^ -((value & 1) as i64)
}

pub struct CompactEncoder {
    cols: usize,
    previous: [Vec<i64>; 2],
}

impl CompactEncoder {
    pub fn new(cols: usize) -> Self {
        Self { cols, previous: [vec![0; cols], vec![0; cols]] }
    }

    pub fn encode(&mut self, interleaved_rows: &[u32]) -> Vec<u8> {
        let cols = self.cols;
        let mut residuals = vec![0u32; interleaved_rows.len()];
        for (index, (current, out)) in interleaved_rows.chunks_exact(cols).zip(residuals.chunks_exact_mut(cols)).enumerate() {
            let (mut left, mut up_left) = (0i64, 0i64);
            for ((&value, above), residual) in current.iter().zip(self.previous[index % 2].iter_mut()).zip(out) {
                let filled = if value == UNREACHABLE_VALUE { 0 } else { value as i64 };
                *residual = zigzag(filled - *above - left + up_left);
                (up_left, left, *above) = (*above, filled, filled);
            }
        }
        interleaved_rows
            .par_chunks(FRAME_ROWS * 2 * cols)
            .zip(residuals.par_chunks(FRAME_ROWS * 2 * cols))
            .map(|(values, residuals)| frame(cols, values, residuals))
            .collect::<Vec<_>>()
            .concat()
    }
}

fn frame(cols: usize, values: &[u32], residuals: &[u32]) -> Vec<u8> {
    let rows = values.len() / (2 * cols);
    let cells = rows * cols;
    let mut payload = vec![0u8; 8 * cells + cells.div_ceil(8)];
    for (row, residual_row) in residuals.chunks_exact(2 * cols).enumerate() {
        for (matrix, matrix_row) in residual_row.chunks_exact(cols).enumerate() {
            for (col, residual) in matrix_row.iter().enumerate() {
                let cell = row * cols + col;
                for (byte, value) in residual.to_le_bytes().into_iter().enumerate() {
                    payload[(4 * matrix + byte) * cells + cell] = value;
                }
            }
        }
    }
    for (cell, _) in values.chunks_exact(2 * cols).flat_map(|row| &row[..cols]).enumerate().filter(|&(_, &value)| value == UNREACHABLE_VALUE) {
        payload[8 * cells + cell / 8] |= 1 << (cell % 8);
    }
    let compressed = zstd::bulk::compress(&payload, ZSTD_LEVEL).expect("zstd compresses in memory");
    [(rows as u32).to_le_bytes(), (compressed.len() as u32).to_le_bytes()].concat().into_iter().chain(compressed).collect()
}

struct Reader<'a> {
    body: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        ensure!(self.offset + len <= self.body.len(), "compact body is truncated");
        self.offset += len;
        Ok(&self.body[self.offset - len..self.offset])
    }

    fn word(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }

    fn words(&mut self, count: usize) -> Result<Vec<u32>> {
        Ok(self.take(4 * count)?.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect())
    }
}

pub fn decode_compact(body: &[u8]) -> Result<DecodedMatrix> {
    let mut reader = Reader { body, offset: 0 };
    ensure!(reader.take(4)? == COMPACT_MAGIC, "not a compact distance matrix body");
    let (rows, cols, frames) = (reader.word()? as usize, reader.word()? as usize, reader.word()?);
    let (row_order, col_order) = (reader.words(rows)?, reader.words(cols)?);
    let mut encoded = [vec![0u32; rows * cols], vec![0u32; rows * cols]];
    let mut previous = [vec![0i64; cols], vec![0i64; cols]];
    let mut first_row = 0;
    for _ in 0..frames {
        let (frame_rows, size) = (reader.word()? as usize, reader.word()? as usize);
        let cells = frame_rows * cols;
        let payload = zstd::bulk::decompress(reader.take(size)?, 8 * cells + cells.div_ceil(8))?;
        ensure!(payload.len() == 8 * cells + cells.div_ceil(8), "compact frame has the wrong size");
        for (matrix, above) in previous.iter_mut().enumerate() {
            for row in 0..frame_rows {
                let (mut left, mut up_left) = (0i64, 0i64);
                for (col, above) in above.iter_mut().enumerate() {
                    let cell = row * cols + col;
                    let zig = u32::from_le_bytes(std::array::from_fn(|byte| payload[(4 * matrix + byte) * cells + cell]));
                    let value = unzigzag(zig) + *above + left - up_left;
                    (up_left, left, *above) = (*above, value, value);
                    let unreachable = (payload[8 * cells + cell / 8] >> (cell % 8)) & 1 == 1;
                    encoded[matrix][(first_row + row) * cols + col] = if unreachable { UNREACHABLE_VALUE } else { value as u32 };
                }
            }
        }
        first_row += frame_rows;
    }
    ensure!(first_row == rows && reader.offset == body.len(), "compact body does not hold a {rows}x{cols} matrix");
    let [mut distances, mut durations] = [vec![0u32; rows * cols], vec![0u32; rows * cols]];
    for (k, &row) in row_order.iter().enumerate() {
        for (l, &col) in col_order.iter().enumerate() {
            let (target, source) = (row as usize * cols + col as usize, k * cols + l);
            (distances[target], durations[target]) = (encoded[0][source], encoded[1][source]);
        }
    }
    Ok(DecodedMatrix { rows, cols, distances, durations })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    #[test]
    fn round_trips_matrices_with_holes_and_intercontinental_values() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(5);
        for (rows, cols) in [(1, 1), (7, 3), (33, 70), (100, 41)] {
            let points: Vec<Coord> = (0..rows).map(|_| Coord::from_degrees(rng.gen_range(-60.0..70.0), rng.gen_range(-180.0..180.0))).collect();
            let row_order = spatial_order(&points);
            let col_order: Vec<u32> = (0..cols as u32).rev().collect();
            let mut interleaved = Vec::with_capacity(rows * 2 * cols);
            for _ in 0..rows {
                let reachable: Vec<bool> = (0..cols).map(|_| rng.gen_bool(0.7)).collect();
                for limit in [60_000_000, 9_000_000] {
                    interleaved.extend(reachable.iter().map(|&r| if r { rng.gen_range(0..limit) } else { UNREACHABLE_VALUE }));
                }
            }
            let mut encoder = CompactEncoder::new(cols);
            let blocks = interleaved.chunks(2 * FRAME_ROWS * 2 * cols).flat_map(|block| encoder.encode(block));
            let body: Vec<u8> = compact_header(&row_order, &col_order).into_iter().chain(blocks).collect();
            let decoded = decode_compact(&body).unwrap();
            for (k, row) in interleaved.chunks_exact(2 * cols).enumerate() {
                for (l, &col) in col_order.iter().enumerate() {
                    let cell = row_order[k] as usize * cols + col as usize;
                    assert_eq!((decoded.distances[cell], decoded.durations[cell]), (row[l], row[cols + l]));
                }
            }
        }
    }
}
