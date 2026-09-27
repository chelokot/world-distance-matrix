use crate::{words, DecodedMatrix, NO_ROUTE};

pub const CONTENT_TYPE: &str = "application/vnd.distance-matrix.compact.v1";
pub const MAGIC: [u8; 4] = *b"DMC1";
pub const FRAME_ROWS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerTimes {
    pub queue_us: u32,
    pub prepare_us: u32,
    pub compute_us: u32,
}

impl ServerTimes {
    pub fn to_bytes(self) -> Vec<u8> {
        [self.queue_us, self.prepare_us, self.compute_us].into_iter().flat_map(u32::to_le_bytes).collect()
    }
}

pub fn zigzag(value: i64) -> u32 {
    ((value << 1) ^ (value >> 63)) as u32
}

fn unzigzag(value: u32) -> i64 {
    (value >> 1) as i64 ^ -((value & 1) as i64)
}

pub fn payload_len(cells: usize) -> usize {
    8 * cells + cells.div_ceil(8)
}

#[derive(Default)]
enum Stage {
    #[default]
    Header,
    Frames(u32),
    Trailer,
    Done(ServerTimes),
}

#[derive(Default)]
pub struct CompactDecoder {
    pending: Vec<u8>,
    stage: Stage,
    matrix: DecodedMatrix,
    previous: [Vec<i64>; 2],
    row_order: Vec<u32>,
    col_order: Vec<u32>,
    decoded_rows: usize,
}

impl CompactDecoder {
    pub fn feed(&mut self, bytes: &[u8], decompress: &mut impl FnMut(&[u8], usize) -> Result<Vec<u8>, String>) -> Result<(), String> {
        let mut pending = std::mem::take(&mut self.pending);
        pending.extend_from_slice(bytes);
        let mut offset = 0;
        loop {
            let available = &pending[offset..];
            match self.stage {
                Stage::Header => {
                    if available.len() < 16 {
                        break;
                    }
                    if available[..4] != MAGIC {
                        return Err("not a compact distance matrix body".into());
                    }
                    let [rows, cols, frames] = words(&available[4..16])[..] else { unreachable!("three words") };
                    let (rows, cols) = (rows as usize, cols as usize);
                    let needed = 16 + 4 * (rows + cols);
                    if available.len() < needed {
                        break;
                    }
                    self.row_order = words(&available[16..16 + 4 * rows]);
                    self.col_order = words(&available[16 + 4 * rows..needed]);
                    self.matrix = DecodedMatrix { rows, cols, distances: vec![NO_ROUTE; rows * cols], durations: vec![NO_ROUTE; rows * cols] };
                    self.previous = [vec![0; cols], vec![0; cols]];
                    offset += needed;
                    self.stage = if frames == 0 { Stage::Trailer } else { Stage::Frames(frames) };
                }
                Stage::Frames(left) => {
                    if available.len() < 8 {
                        break;
                    }
                    let [frame_rows, size] = words(&available[..8])[..] else { unreachable!("two words") };
                    let (frame_rows, size) = (frame_rows as usize, size as usize);
                    if available.len() < 8 + size {
                        break;
                    }
                    let payload = decompress(&available[8..8 + size], payload_len(frame_rows * self.matrix.cols))?;
                    self.decode_frame(frame_rows, &payload)?;
                    offset += 8 + size;
                    self.stage = if left == 1 { Stage::Trailer } else { Stage::Frames(left - 1) };
                }
                Stage::Trailer => {
                    if available.len() < 12 {
                        break;
                    }
                    let [queue_us, prepare_us, compute_us] = words(&available[..12])[..] else { unreachable!("three words") };
                    offset += 12;
                    self.stage = Stage::Done(ServerTimes { queue_us, prepare_us, compute_us });
                }
                Stage::Done(_) if available.is_empty() => break,
                Stage::Done(_) => return Err("unexpected bytes after the matrix".into()),
            }
        }
        self.pending = pending[offset..].to_vec();
        Ok(())
    }

    fn decode_frame(&mut self, frame_rows: usize, payload: &[u8]) -> Result<(), String> {
        let Self { matrix, previous, row_order, col_order, decoded_rows, .. } = self;
        let cols = matrix.cols;
        let cells = frame_rows * cols;
        if payload.len() != payload_len(cells) || *decoded_rows + frame_rows > matrix.rows {
            return Err("compact frame does not match the matrix".into());
        }
        for (index, (above, target)) in previous.iter_mut().zip([&mut matrix.distances, &mut matrix.durations]).enumerate() {
            for row in 0..frame_rows {
                let base = row_order[*decoded_rows + row] as usize * cols;
                let (mut left, mut up_left) = (0i64, 0i64);
                for (col, above) in above.iter_mut().enumerate() {
                    let cell = row * cols + col;
                    let zig = u32::from_le_bytes(std::array::from_fn(|byte| payload[(4 * index + byte) * cells + cell]));
                    let value = unzigzag(zig) + *above + left - up_left;
                    (up_left, left, *above) = (*above, value, value);
                    let no_route = (payload[8 * cells + cell / 8] >> (cell % 8)) & 1 == 1;
                    target[base + col_order[col] as usize] = if no_route { NO_ROUTE } else { value as u32 };
                }
            }
        }
        *decoded_rows += frame_rows;
        Ok(())
    }

    pub fn finish(self) -> Result<(DecodedMatrix, ServerTimes), String> {
        match self.stage {
            Stage::Done(times) if self.decoded_rows == self.matrix.rows => Ok((self.matrix, times)),
            _ => Err("compact body is truncated".into()),
        }
    }
}

pub fn decode(body: &[u8], mut decompress: impl FnMut(&[u8], usize) -> Result<Vec<u8>, String>) -> Result<(DecodedMatrix, ServerTimes), String> {
    let mut decoder = CompactDecoder::default();
    decoder.feed(body, &mut decompress)?;
    decoder.finish()
}
