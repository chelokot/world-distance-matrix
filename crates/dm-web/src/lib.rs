use std::cell::RefCell;

use dm_wire::compact::CompactDecoder;
use dm_wire::{binary, DecodedMatrix, RowSink, NO_ROUTE};

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

const DISTANCE_BIN_M: u32 = 100;
const DURATION_BIN_S: u32 = 60;

#[derive(Default)]
pub struct Stats {
    square: bool,
    pairs: u64,
    routed: u64,
    distances: Vec<u32>,
    durations: Vec<u32>,
    longest: [u32; 2],
}

fn median(bins: &[u32], count: u64, width: u32) -> f64 {
    let mut seen = 0u64;
    let bin = bins.iter().position(|&in_bin| {
        seen += in_bin as u64;
        2 * seen > count
    });
    (bin.unwrap_or(0) as u32 * width + width / 2) as f64
}

impl Stats {
    pub fn of(matrix: &DecodedMatrix) -> Self {
        let mut stats = Stats::default();
        stats.start(matrix.rows, matrix.cols);
        for row in 0..matrix.rows {
            let cells = row * matrix.cols..(row + 1) * matrix.cols;
            stats.row(row, &matrix.distances[cells.clone()], &matrix.durations[cells]);
        }
        stats
    }

    pub fn summary(&self) -> [f64; 5] {
        if self.routed == 0 {
            return [0.0; 5];
        }
        let [distance, duration] = [(&self.distances, DISTANCE_BIN_M), (&self.durations, DURATION_BIN_S)].map(|(bins, width)| median(bins, self.routed, width));
        [self.routed as f64 / self.pairs as f64, distance, duration, self.longest[0] as f64, self.longest[1] as f64]
    }
}

impl RowSink for Stats {
    fn start(&mut self, rows: usize, cols: usize) {
        *self = Stats { square: rows == cols, distances: vec![0; 700_000], durations: vec![0; 200_000], ..Stats::default() };
    }

    fn row(&mut self, index: usize, distances: &[u32], durations: &[u32]) {
        for (col, (&distance, &duration)) in distances.iter().zip(durations).enumerate() {
            if self.square && col == index {
                continue;
            }
            self.pairs += 1;
            if distance == NO_ROUTE {
                continue;
            }
            self.routed += 1;
            let last = self.distances.len() - 1;
            self.distances[((distance / DISTANCE_BIN_M) as usize).min(last)] += 1;
            let last = self.durations.len() - 1;
            self.durations[((duration / DURATION_BIN_S) as usize).min(last)] += 1;
            self.longest = [self.longest[0].max(distance), self.longest[1].max(duration)];
        }
    }
}

struct Sink<'a> {
    stats: &'a mut Stats,
    matrix: Option<&'a mut DecodedMatrix>,
}

impl RowSink for Sink<'_> {
    fn start(&mut self, rows: usize, cols: usize) {
        self.stats.start(rows, cols);
        if let Some(matrix) = &mut self.matrix {
            matrix.start(rows, cols);
        }
    }

    fn row(&mut self, index: usize, distances: &[u32], durations: &[u32]) {
        self.stats.row(index, distances, durations);
        if let Some(matrix) = &mut self.matrix {
            matrix.row(index, distances, durations);
        }
    }
}

#[derive(Default)]
struct State {
    input: Vec<u8>,
    stream: CompactDecoder,
    retain: bool,
    stats: Option<Stats>,
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
            (state.matrix, state.times, state.stats) = (matrix, [0; 3], None);
            1
        }
        Err(_) => 0,
    })
}

#[no_mangle]
pub extern "C" fn stream_start(retain: u32) {
    STATE.with_borrow_mut(|state| {
        (state.stream, state.retain, state.stats) = (CompactDecoder::default(), retain == 1, Some(Stats::default()));
        state.matrix = DecodedMatrix::default();
    })
}

#[no_mangle]
pub extern "C" fn stream_feed() -> u32 {
    STATE.with_borrow_mut(|state| {
        let State { input, stream, retain, stats, matrix, .. } = state;
        let stats = stats.as_mut().expect("stream_start comes first");
        stream.feed(input, &mut Sink { stats, matrix: retain.then_some(matrix) }).is_ok() as u32
    })
}

#[no_mangle]
pub extern "C" fn stream_complete() -> u32 {
    STATE.with_borrow(|state| state.stream.is_complete() as u32)
}

#[no_mangle]
pub extern "C" fn stream_finish() -> u32 {
    STATE.with_borrow_mut(|state| match std::mem::take(&mut state.stream).finish() {
        Ok(times) => {
            state.times = [times.queue_us, times.prepare_us, times.compute_us];
            1
        }
        Err(_) => 0,
    })
}

#[no_mangle]
pub extern "C" fn prepare_matrix(rows: u32, cols: u32) {
    STATE.with_borrow_mut(|state| {
        state.matrix.start(rows as usize, cols as usize);
        state.stats = None;
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
        state.summary = state.stats.as_ref().map_or_else(|| Stats::of(&state.matrix).summary(), Stats::summary);
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
    use dm_wire::compact::{decode, encode_frame, encode_header, Axis, Layout, ServerTimes};

    #[test]
    fn streams_what_the_server_encodes() {
        let (rows, cols) = (70, 45);
        let axis =
            |len: u32| Axis { order: (0..len).rev().collect(), parents: (0..len).map(|index| (index.saturating_sub(3)..index).rev().collect()).collect() };
        let layout = Layout { rows: axis(rows), cols: Some(axis(cols)) };
        let cell =
            |row: usize, col: usize| (!(row + col).is_multiple_of(11)).then(|| [(row * 7_919 + col * 104_729) as i64, (row * 31 + col * 977) as i64 * 1_000]);
        let times = ServerTimes { queue_us: 3, prepare_us: 400, compute_us: 9_000 };
        let frames = (0..layout.frames()).flat_map(|frame| encode_frame(&layout, frame, cell));
        let body: Vec<u8> = encode_header(&layout).into_iter().chain(frames).chain(times.to_bytes()).collect();
        stream_start(1);
        for piece in body.chunks(1_000) {
            assert_eq!(stream_complete(), 0);
            STATE.with_borrow_mut(|state| state.input = piece.to_vec());
            assert_eq!(stream_feed(), 1);
        }
        assert_eq!(stream_complete(), 1);
        assert_eq!(stream_finish(), 1);
        let (expected, _) = decode(&body).unwrap();
        STATE.with_borrow(|state| {
            assert_eq!(state.matrix, expected);
            assert_eq!(state.times, [3, 400, 9_000]);
            assert_eq!(state.stats.as_ref().unwrap().summary(), Stats::of(&expected).summary());
        });
        stream_start(0);
        STATE.with_borrow_mut(|state| state.input = body.clone());
        assert_eq!((stream_feed(), stream_finish()), (1, 1));
        STATE.with_borrow(|state| assert_eq!((state.matrix.rows, state.stats.as_ref().unwrap().summary()), (0, Stats::of(&expected).summary())));
    }

    #[test]
    fn statistics_skip_the_diagonal_and_report_medians_to_their_bin() {
        let matrix = DecodedMatrix { rows: 2, cols: 2, distances: vec![0, 12_345, NO_ROUTE, 0], durations: vec![0, 4_000, NO_ROUTE, 0] };
        assert_eq!(Stats::of(&matrix).summary(), [0.5, 12_350.0, 3_990.0, 12_345.0, 4_000.0]);
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
