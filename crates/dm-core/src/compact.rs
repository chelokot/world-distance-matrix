use anyhow::Result;
use dm_wire::compact::{payload_len, zigzag, ServerTimes, MAGIC};
use dm_wire::{DecodedMatrix, NO_ROUTE};
use flate2::{Compress, Compression, Crc, FlushCompress};

use crate::geo::{hilbert_index, Coord};

const ZSTD_LEVEL: i32 = 3;
const DEFLATE_LEVEL: u32 = 4;
const GZIP_HEADER: [u8; 10] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
const FINAL_EMPTY_DEFLATE_BLOCK: [u8; 2] = [3, 0];

pub fn spatial_order(points: &[Coord]) -> Vec<u32> {
    let mut order: Vec<u32> = (0..points.len() as u32).collect();
    order.sort_by_key(|&index| (hilbert_index(points[index as usize]), index));
    order
}

pub fn compact_header(row_order: &[u32], col_order: &[u32]) -> Vec<u8> {
    let (rows, cols) = (row_order.len(), col_order.len());
    let frames = rows.div_ceil(dm_wire::compact::FRAME_ROWS) as u32;
    let words = [rows as u32, cols as u32, frames].into_iter().chain(row_order.iter().copied()).chain(col_order.iter().copied());
    MAGIC.into_iter().chain(words.flat_map(u32::to_le_bytes)).collect()
}

pub fn encode_frame(cols: usize, row_above: Option<&[u32]>, interleaved_rows: &[u32]) -> Vec<u8> {
    let rows = interleaved_rows.len() / (2 * cols);
    let cells = rows * cols;
    let filled = |value: u32| if value == NO_ROUTE { 0 } else { value as i64 };
    let mut frame = vec![0u8; 4 + payload_len(cells)];
    frame[..4].copy_from_slice(&(rows as u32).to_le_bytes());
    let (planes, no_route) = frame[4..].split_at_mut(8 * cells);
    for matrix in 0..2 {
        let mut above: Vec<i64> = row_above.map_or_else(|| vec![0; cols], |row| row[matrix * cols..(matrix + 1) * cols].iter().map(|&v| filled(v)).collect());
        for (row, values) in interleaved_rows.chunks_exact(2 * cols).enumerate() {
            let (mut left, mut up_left) = (0i64, 0i64);
            for (col, (&value, above)) in values[matrix * cols..(matrix + 1) * cols].iter().zip(above.iter_mut()).enumerate() {
                let value = filled(value);
                let residual = zigzag(value - *above - left + up_left).to_le_bytes();
                (up_left, left, *above) = (*above, value, value);
                for (byte, residual_byte) in residual.into_iter().enumerate() {
                    planes[(4 * matrix + byte) * cells + row * cols + col] = residual_byte;
                }
            }
        }
    }
    for (cell, _) in interleaved_rows.chunks_exact(2 * cols).flat_map(|row| &row[..cols]).enumerate().filter(|&(_, &value)| value == NO_ROUTE) {
        no_route[cell / 8] |= 1 << (cell % 8);
    }
    frame
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Identity,
    Zstd,
    Gzip,
}

pub struct Piece {
    bytes: Vec<u8>,
    crc: Crc,
}

impl Transport {
    pub fn content_encoding(self) -> Option<&'static str> {
        match self {
            Transport::Identity => None,
            Transport::Zstd => Some("zstd"),
            Transport::Gzip => Some("gzip"),
        }
    }

    pub fn encode(self, raw: Vec<u8>) -> Piece {
        let mut crc = Crc::new();
        let bytes = match self {
            Transport::Identity => raw,
            Transport::Zstd => zstd::bulk::compress(&raw, ZSTD_LEVEL).expect("zstd compresses in memory"),
            Transport::Gzip => {
                crc.update(&raw);
                deflate_segment(&raw)
            }
        };
        Piece { bytes, crc }
    }
}

fn deflate_segment(bytes: &[u8]) -> Vec<u8> {
    let mut compress = Compress::new(Compression::new(DEFLATE_LEVEL), false);
    let mut segment = Vec::with_capacity(bytes.len() / 2 + 64);
    loop {
        let consumed = compress.total_in() as usize;
        compress.compress_vec(&bytes[consumed..], &mut segment, FlushCompress::Sync).expect("deflate into memory");
        if compress.total_in() as usize == bytes.len() && segment.len() < segment.capacity() {
            return segment;
        }
        segment.reserve(segment.capacity());
    }
}

pub struct TransportEncoder {
    transport: Transport,
    crc: Crc,
}

impl TransportEncoder {
    pub fn new(transport: Transport) -> Self {
        Self { transport, crc: Crc::new() }
    }

    pub fn start(&self) -> Vec<u8> {
        if self.transport == Transport::Gzip {
            GZIP_HEADER.to_vec()
        } else {
            Vec::new()
        }
    }

    pub fn push(&mut self, piece: Piece) -> Vec<u8> {
        self.crc.combine(&piece.crc);
        piece.bytes
    }

    pub fn finish(self) -> Vec<u8> {
        if self.transport != Transport::Gzip {
            return Vec::new();
        }
        FINAL_EMPTY_DEFLATE_BLOCK.into_iter().chain(self.crc.sum().to_le_bytes()).chain(self.crc.amount().to_le_bytes()).collect()
    }
}

pub fn decode_compact(body: &[u8]) -> Result<(DecodedMatrix, ServerTimes)> {
    dm_wire::compact::decode(body).map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use dm_wire::compact::FRAME_ROWS;
    use rand::{Rng, SeedableRng};

    fn encode_frames(cols: usize, interleaved: &[u32]) -> Vec<u8> {
        let row = 2 * cols;
        interleaved
            .chunks(FRAME_ROWS * row)
            .enumerate()
            .flat_map(|(index, frame)| encode_frame(cols, (index > 0).then(|| &interleaved[(index * FRAME_ROWS - 1) * row..index * FRAME_ROWS * row]), frame))
            .collect()
    }

    #[test]
    fn transports_make_one_valid_stream_from_large_pieces() {
        let pieces: Vec<Vec<u8>> =
            (0..3u32).map(|piece| (0..600_000u32).map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(piece) >> 29) as u8).collect()).collect();
        for transport in [Transport::Identity, Transport::Zstd, Transport::Gzip] {
            let mut wire = TransportEncoder::new(transport);
            let mut body = wire.start();
            for piece in &pieces {
                body.extend(wire.push(transport.encode(piece.clone())));
            }
            body.extend(wire.finish());
            let decoded = match transport {
                Transport::Identity => body,
                Transport::Zstd => zstd::stream::decode_all(&body[..]).unwrap(),
                Transport::Gzip => {
                    let mut decoded = Vec::new();
                    flate2::read::GzDecoder::new(&body[..]).read_to_end(&mut decoded).unwrap();
                    decoded
                }
            };
            assert!(decoded == pieces.concat(), "{transport:?}");
        }
    }

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
            let times = ServerTimes { queue_us: 1, prepare_us: 20, compute_us: 300 };
            let body: Vec<u8> = [compact_header(&row_order, &col_order), encode_frames(cols, &interleaved), times.to_bytes()].concat();
            let (decoded, decoded_times) = decode_compact(&body).unwrap();
            assert_eq!(decoded_times, times);
            let mut streamed = dm_wire::compact::CompactDecoder::default();
            for piece in body.chunks(7) {
                streamed.feed(piece).unwrap();
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
