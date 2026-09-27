use anyhow::Result;
use dm_wire::compact::{payload_len, zigzag, ServerTimes, FRAME_ROWS, MAGIC};
use dm_wire::{DecodedMatrix, NO_ROUTE};
use rayon::prelude::*;

use crate::geo::{hilbert_index, Coord};

const ZSTD_LEVEL: i32 = 3;

pub fn spatial_order(points: &[Coord]) -> Vec<u32> {
    let mut order: Vec<u32> = (0..points.len() as u32).collect();
    order.sort_by_key(|&index| (hilbert_index(points[index as usize]), index));
    order
}

pub fn compact_header(row_order: &[u32], col_order: &[u32]) -> Vec<u8> {
    let (rows, cols) = (row_order.len(), col_order.len());
    let words = [rows as u32, cols as u32, rows.div_ceil(FRAME_ROWS) as u32].into_iter().chain(row_order.iter().copied()).chain(col_order.iter().copied());
    MAGIC.into_iter().chain(words.flat_map(u32::to_le_bytes)).collect()
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
                let filled = if value == NO_ROUTE { 0 } else { value as i64 };
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
    let mut payload = vec![0u8; payload_len(cells)];
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
    for (cell, _) in values.chunks_exact(2 * cols).flat_map(|row| &row[..cols]).enumerate().filter(|&(_, &value)| value == NO_ROUTE) {
        payload[8 * cells + cell / 8] |= 1 << (cell % 8);
    }
    let compressed = zstd::bulk::compress(&payload, ZSTD_LEVEL).expect("zstd compresses in memory");
    [(rows as u32).to_le_bytes(), (compressed.len() as u32).to_le_bytes()].concat().into_iter().chain(compressed).collect()
}

pub fn decode_compact(body: &[u8]) -> Result<(DecodedMatrix, ServerTimes)> {
    dm_wire::compact::decode(body, |frame, len| zstd::bulk::decompress(frame, len).map_err(|e| e.to_string())).map_err(anyhow::Error::msg)
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
                    interleaved.extend(reachable.iter().map(|&r| if r { rng.gen_range(0..limit) } else { NO_ROUTE }));
                }
            }
            let mut encoder = CompactEncoder::new(cols);
            let blocks = interleaved.chunks(2 * FRAME_ROWS * 2 * cols).flat_map(|block| encoder.encode(block));
            let times = ServerTimes { queue_us: 1, prepare_us: 20, compute_us: 300 };
            let body: Vec<u8> = compact_header(&row_order, &col_order).into_iter().chain(blocks).chain(times.to_bytes()).collect();
            let (decoded, decoded_times) = decode_compact(&body).unwrap();
            assert_eq!(decoded_times, times);
            let mut streamed = dm_wire::compact::CompactDecoder::default();
            let mut decompress = |frame: &[u8], len: usize| zstd::bulk::decompress(frame, len).map_err(|e| e.to_string());
            for piece in body.chunks(7) {
                streamed.feed(piece, &mut decompress).unwrap();
            }
            let (streamed_matrix, streamed_times) = streamed.finish().unwrap();
            assert_eq!((&streamed_matrix, streamed_times), (&decoded, times));
            for (k, row) in interleaved.chunks_exact(2 * cols).enumerate() {
                for (l, &col) in col_order.iter().enumerate() {
                    let cell = row_order[k] as usize * cols + col as usize;
                    assert_eq!((decoded.distances[cell], decoded.durations[cell]), (row[l], row[cols + l]));
                }
            }
        }
    }
}
