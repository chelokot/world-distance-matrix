pub mod binary;
pub mod compact;

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

struct Reader<'a> {
    body: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        if self.offset + len > self.body.len() {
            return Err("body is truncated".into());
        }
        self.offset += len;
        Ok(&self.body[self.offset - len..self.offset])
    }

    fn words(&mut self, count: usize) -> Result<Vec<u32>, String> {
        Ok(self.take(4 * count)?.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect())
    }

    fn word(&mut self) -> Result<u32, String> {
        Ok(self.words(1)?[0])
    }

    fn finished(&self) -> bool {
        self.offset == self.body.len()
    }
}
