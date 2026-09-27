use crate::{words, DecodedMatrix, NO_ROUTE};

pub const CONTENT_TYPE: &str = "application/vnd.distance-matrix.v1";
pub const MAGIC: [u8; 4] = *b"DMX1";
pub const HEADER_LEN: usize = 16;

pub fn header(rows: u32, cols: u32) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4..8].copy_from_slice(&NO_ROUTE.to_le_bytes());
    header[8..12].copy_from_slice(&rows.to_le_bytes());
    header[12..16].copy_from_slice(&cols.to_le_bytes());
    header
}

pub fn len(rows: usize, cols: usize) -> usize {
    HEADER_LEN + rows * cols * 2 * size_of::<u32>()
}

pub fn decode(body: &[u8]) -> Result<DecodedMatrix, String> {
    if body.len() < HEADER_LEN || body[..4] != MAGIC {
        return Err("not a binary distance matrix body".into());
    }
    let [no_route, rows, cols] = words(&body[4..HEADER_LEN])[..] else { unreachable!("three words") };
    let (rows, cols) = (rows as usize, cols as usize);
    if no_route != NO_ROUTE || body.len() != len(rows, cols) {
        return Err(format!("body does not hold a {rows}x{cols} matrix"));
    }
    let values = words(&body[HEADER_LEN..]);
    let mut distances = Vec::with_capacity(rows * cols);
    let mut durations = Vec::with_capacity(rows * cols);
    for row in 0..rows {
        let start = row * 2 * cols;
        distances.extend_from_slice(&values[start..start + cols]);
        durations.extend_from_slice(&values[start + cols..start + 2 * cols]);
    }
    Ok(DecodedMatrix { rows, cols, distances, durations })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let mut body = header(2, 3).to_vec();
        let rows: [u32; 12] = [1, 2, 3, 10, 20, 30, 4, NO_ROUTE, 6, 40, NO_ROUTE, 60];
        body.extend(rows.iter().flat_map(|v| v.to_le_bytes()));
        let decoded = decode(&body).unwrap();
        assert_eq!(decoded.distances, vec![1, 2, 3, 4, NO_ROUTE, 6]);
        assert_eq!(decoded.durations, vec![10, 20, 30, 40, NO_ROUTE, 60]);
        assert_eq!(decoded.distance(1, 1), None);
        assert_eq!(decoded.duration(1, 2), Some(60));
    }

    #[test]
    fn rejects_truncated_bodies() {
        let mut body = header(2, 2).to_vec();
        body.extend([0u8; 12]);
        assert!(decode(&body).is_err());
    }
}
