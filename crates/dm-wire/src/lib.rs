pub mod binary;
pub mod coder;
pub mod compact;
pub mod request;

pub const NO_ROUTE: u32 = u32::MAX;

pub fn meters(decimeters: u64) -> u32 {
    ((decimeters + 5) >> 1) as u32 / 5
}

pub fn seconds(milliseconds: u64) -> u32 {
    ((milliseconds + 500) >> 3) as u32 / 125
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DecodedMatrix {
    pub rows: usize,
    pub cols: usize,
    pub distances: Vec<u32>,
    pub durations: Vec<u32>,
}

impl DecodedMatrix {
    pub fn distance(&self, row: usize, col: usize) -> Option<u32> {
        Some(self.distances[row * self.cols + col]).filter(|&v| v != NO_ROUTE)
    }

    pub fn duration(&self, row: usize, col: usize) -> Option<u32> {
        Some(self.durations[row * self.cols + col]).filter(|&v| v != NO_ROUTE)
    }
}

pub trait RowSink {
    fn start(&mut self, rows: usize, cols: usize);

    fn row(&mut self, index: usize, distances: &[u32], durations: &[u32]);
}

impl RowSink for DecodedMatrix {
    fn start(&mut self, rows: usize, cols: usize) {
        *self = DecodedMatrix { rows, cols, distances: vec![NO_ROUTE; rows * cols], durations: vec![NO_ROUTE; rows * cols] };
    }

    fn row(&mut self, index: usize, distances: &[u32], durations: &[u32]) {
        let cells = index * self.cols..(index + 1) * self.cols;
        self.distances[cells.clone()].copy_from_slice(distances);
        self.durations[cells].copy_from_slice(durations);
    }
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_matches_plain_division_across_the_weight_range() {
        let samples = (0..200_000u64).chain((0..33).flat_map(|bits| [(1u64 << bits) - 1, 1 << bits, (1 << bits) + 499, (1 << bits) + 500]));
        for value in samples.chain([(1 << 29) - 1, (1 << 33) - 1]) {
            assert_eq!(meters(value.min((1 << 29) - 1)) as u64, (value.min((1 << 29) - 1) + 5) / 10);
            assert_eq!(seconds(value) as u64, (value + 500) / 1000, "{value}");
        }
    }
}
