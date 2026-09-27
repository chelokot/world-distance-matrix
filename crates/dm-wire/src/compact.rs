use crate::{DecodedMatrix, Reader, NO_ROUTE};

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

pub fn decode(body: &[u8], mut decompress: impl FnMut(&[u8], usize) -> Result<Vec<u8>, String>) -> Result<(DecodedMatrix, ServerTimes), String> {
    let mut reader = Reader { body, offset: 0 };
    if reader.take(4)? != MAGIC {
        return Err("not a compact distance matrix body".into());
    }
    let (rows, cols, frames) = (reader.word()? as usize, reader.word()? as usize, reader.word()?);
    let (row_order, col_order) = (reader.words(rows)?, reader.words(cols)?);
    let mut encoded = [vec![0u32; rows * cols], vec![0u32; rows * cols]];
    let mut previous = [vec![0i64; cols], vec![0i64; cols]];
    let mut first_row = 0;
    for _ in 0..frames {
        let (frame_rows, size) = (reader.word()? as usize, reader.word()? as usize);
        let cells = frame_rows * cols;
        let payload = decompress(reader.take(size)?, payload_len(cells))?;
        if payload.len() != payload_len(cells) || first_row + frame_rows > rows {
            return Err("compact frame does not match the matrix".into());
        }
        for (matrix, above) in previous.iter_mut().enumerate() {
            for row in 0..frame_rows {
                let (mut left, mut up_left) = (0i64, 0i64);
                for (col, above) in above.iter_mut().enumerate() {
                    let cell = row * cols + col;
                    let zig = u32::from_le_bytes(std::array::from_fn(|byte| payload[(4 * matrix + byte) * cells + cell]));
                    let value = unzigzag(zig) + *above + left - up_left;
                    (up_left, left, *above) = (*above, value, value);
                    let no_route = (payload[8 * cells + cell / 8] >> (cell % 8)) & 1 == 1;
                    encoded[matrix][(first_row + row) * cols + col] = if no_route { NO_ROUTE } else { value as u32 };
                }
            }
        }
        first_row += frame_rows;
    }
    let [queue_us, prepare_us, compute_us] = std::array::from_fn(|_| reader.word());
    let times = ServerTimes { queue_us: queue_us?, prepare_us: prepare_us?, compute_us: compute_us? };
    if first_row != rows || !reader.finished() {
        return Err(format!("compact body does not hold a {rows}x{cols} matrix"));
    }
    let [mut distances, mut durations] = [vec![0u32; rows * cols], vec![0u32; rows * cols]];
    for (k, &row) in row_order.iter().enumerate() {
        for (l, &col) in col_order.iter().enumerate() {
            let (target, source) = (row as usize * cols + col as usize, k * cols + l);
            (distances[target], durations[target]) = (encoded[0][source], encoded[1][source]);
        }
    }
    Ok((DecodedMatrix { rows, cols, distances, durations }, times))
}
