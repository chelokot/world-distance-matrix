use std::cell::RefCell;

use dm_wire::compact::CompactDecoder;
use dm_wire::{binary, DecodedMatrix, NO_ROUTE};

pub fn in_eurasia(lat: f64, lon: f64) -> bool {
    let in_box = (1.0..=75.0).contains(&lat) && (-10.5..=180.0).contains(&lon);
    let north_african_coast = match lon {
        lon if lon < -5.5 => 36.5,
        lon if lon < -1.0 => 35.95,
        lon if lon < 8.5 => 37.2,
        lon if lon < 11.5 => 37.5,
        _ => 34.0,
    };
    let red_sea = 32.5 + (30.0 - lat) * (43.4 - 32.5) / (30.0 - 12.6);
    let africa = if lon < 32.5 {
        lat < north_african_coast
    } else if lat >= 12.6 {
        lat < 30.0 && lon < red_sea
    } else {
        lon < 51.5
    };
    in_box && !africa
}

pub fn eurasia_lattice(lattice: u32, rotation_deg: f64) -> Vec<(f64, f64)> {
    let golden_angle = 180.0 * (3.0 - 5f64.sqrt());
    (0..lattice)
        .map(|k| {
            let z = 1.0 - (2 * k + 1) as f64 / lattice as f64;
            (z.asin().to_degrees(), (k as f64 * golden_angle + rotation_deg).rem_euclid(360.0) - 180.0)
        })
        .filter(|&(lat, lon)| in_eurasia(lat, lon))
        .collect()
}

pub fn summarize(matrix: &DecodedMatrix) -> [f64; 5] {
    let (mut distances, mut durations) = (Vec::new(), Vec::new());
    let mut pairs = 0usize;
    for row in 0..matrix.rows {
        for col in (0..matrix.cols).filter(|&col| matrix.rows != matrix.cols || col != row) {
            pairs += 1;
            if let (Some(distance), Some(duration)) = (matrix.distance(row, col), matrix.duration(row, col)) {
                distances.push(distance);
                durations.push(duration);
            }
        }
    }
    if distances.is_empty() {
        return [0.0; 5];
    }
    let median = |values: &mut Vec<u32>| {
        let middle = values.len() / 2;
        *values.select_nth_unstable(middle).1 as f64
    };
    let max = |values: &[u32]| *values.iter().max().expect("non-empty") as f64;
    [distances.len() as f64 / pairs as f64, median(&mut distances), median(&mut durations), max(&distances), max(&durations)]
}

#[derive(Default)]
struct State {
    input: Vec<u8>,
    stream: CompactDecoder,
    matrix: DecodedMatrix,
    times: [u32; 3],
    points: Vec<f64>,
    summary: [f64; 5],
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

#[no_mangle]
pub extern "C" fn input(len: usize) -> *mut u8 {
    STATE.with_borrow_mut(|state| {
        state.input = vec![0; len];
        state.input.as_mut_ptr()
    })
}

#[no_mangle]
pub extern "C" fn decode_binary() -> u32 {
    STATE.with_borrow_mut(|state| match binary::decode(&state.input) {
        Ok(matrix) => {
            (state.matrix, state.times) = (matrix, [0; 3]);
            1
        }
        Err(_) => 0,
    })
}

#[no_mangle]
pub extern "C" fn stream_start() {
    STATE.with_borrow_mut(|state| state.stream = CompactDecoder::default())
}

#[no_mangle]
pub extern "C" fn stream_feed() -> u32 {
    STATE.with_borrow_mut(|state| state.stream.feed(&state.input).is_ok() as u32)
}

#[no_mangle]
pub extern "C" fn stream_complete() -> u32 {
    STATE.with_borrow(|state| state.stream.is_complete() as u32)
}

#[no_mangle]
pub extern "C" fn stream_finish() -> u32 {
    STATE.with_borrow_mut(|state| match std::mem::take(&mut state.stream).finish() {
        Ok((matrix, times)) => {
            (state.matrix, state.times) = (matrix, [times.queue_us, times.prepare_us, times.compute_us]);
            1
        }
        Err(_) => 0,
    })
}

#[no_mangle]
pub extern "C" fn prepare_matrix(rows: u32, cols: u32) {
    STATE.with_borrow_mut(|state| {
        let cells = rows as usize * cols as usize;
        state.matrix = DecodedMatrix { rows: rows as usize, cols: cols as usize, distances: vec![NO_ROUTE; cells], durations: vec![NO_ROUTE; cells] };
    })
}

#[no_mangle]
pub extern "C" fn distances() -> *mut u32 {
    STATE.with_borrow_mut(|state| state.matrix.distances.as_mut_ptr())
}

#[no_mangle]
pub extern "C" fn durations() -> *mut u32 {
    STATE.with_borrow_mut(|state| state.matrix.durations.as_mut_ptr())
}

#[no_mangle]
pub extern "C" fn server_times() -> *const u32 {
    STATE.with_borrow(|state| state.times.as_ptr())
}

#[no_mangle]
pub extern "C" fn summary() -> *const f64 {
    STATE.with_borrow_mut(|state| {
        state.summary = summarize(&state.matrix);
        state.summary.as_ptr()
    })
}

#[no_mangle]
pub extern "C" fn eurasia(lattice: u32, rotation_deg: f64) -> u32 {
    STATE.with_borrow_mut(|state| {
        state.points = eurasia_lattice(lattice, rotation_deg).into_iter().flat_map(|(lat, lon)| [lat, lon]).collect();
        (state.points.len() / 2) as u32
    })
}

#[no_mangle]
pub extern "C" fn points() -> *const f64 {
    STATE.with_borrow(|state| state.points.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dm_core::compact::{compact_header, encode_frame};
    use dm_wire::compact::{ServerTimes, FRAME_ROWS};

    #[test]
    fn streams_what_the_server_encodes() {
        let (rows, cols) = (70, 45);
        let interleaved: Vec<u32> = (0..rows)
            .flat_map(|row| {
                let distances: Vec<u32> =
                    (0..cols).map(|col| if (row + col) % 11 == 0 { NO_ROUTE } else { ((row * 7_919 + col * 104_729) % 40_000_000) as u32 }).collect();
                let durations: Vec<u32> = distances.iter().map(|&d| if d == NO_ROUTE { NO_ROUTE } else { d / 17 }).collect();
                distances.into_iter().chain(durations)
            })
            .collect();
        let (row_order, col_order): (Vec<u32>, Vec<u32>) = ((0..rows as u32).rev().collect(), (0..cols as u32).collect());
        let times = ServerTimes { queue_us: 3, prepare_us: 400, compute_us: 9_000 };
        let row = 2 * cols;
        let frames: Vec<u8> = interleaved
            .chunks(FRAME_ROWS * row)
            .enumerate()
            .flat_map(|(index, frame)| encode_frame(cols, (index > 0).then(|| &interleaved[(index * FRAME_ROWS - 1) * row..index * FRAME_ROWS * row]), frame))
            .collect();
        let body = [compact_header(&row_order, &col_order), frames, times.to_bytes()].concat();
        stream_start();
        for piece in body.chunks(1_000) {
            assert_eq!(stream_complete(), 0);
            STATE.with_borrow_mut(|state| state.input = piece.to_vec());
            assert_eq!(stream_feed(), 1);
        }
        assert_eq!(stream_complete(), 1);
        assert_eq!(stream_finish(), 1);
        let (expected, _) = dm_core::compact::decode_compact(&body).unwrap();
        STATE.with_borrow(|state| {
            assert_eq!(state.matrix, expected);
            assert_eq!(state.times, [3, 400, 9_000]);
        });
    }

    #[test]
    fn eurasian_lattice_avoids_africa_and_other_continents() {
        for (lat, lon, inside) in
            [(52.5, 13.4, true), (1.35, 103.8, true), (35.7, 139.7, true), (24.7, 46.7, true), (30.0, 31.2, false), (9.0, 38.7, false), (40.7, -74.0, false)]
        {
            assert_eq!(in_eurasia(lat, lon), inside, "{lat},{lon}");
        }
        let points = eurasia_lattice(20_000, 0.0);
        assert!((4_000..4_400).contains(&points.len()), "{}", points.len());
    }
}
