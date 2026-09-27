use rayon::prelude::*;

use crate::weight::UNREACHABLE_VALUE;

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
}
