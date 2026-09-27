pub mod binary;
pub mod compact;
pub mod request;

pub const NO_ROUTE: u32 = u32::MAX;

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

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect()
}
