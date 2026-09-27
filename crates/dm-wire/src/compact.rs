use std::ops::Range;

use crate::coder::{section_len, significant_bits, unzigzag, zigzag, Bit, Coder, Decoder, Encoder, SECTION_HEADER_LEN};
use crate::{meters, seconds, words, DecodedMatrix, RowSink, NO_ROUTE};

pub const CONTENT_TYPE: &str = "application/vnd.distance-matrix.compact.v2";
pub const MAGIC: [u8; 4] = *b"DMC2";
pub const FRAME_ROWS: usize = 32;
pub const PARENTS: usize = 8;
pub const PARENT_WINDOW: usize = 1024;
const HEADER_LEN: usize = 16;
const TRAILER_LEN: usize = 12;
const VALUE_LIMIT: i64 = 1 << 40;

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Axis {
    pub order: Vec<u32>,
    pub parents: Vec<Vec<u32>>,
}

impl Axis {
    fn first_parent(&self, index: usize) -> Option<usize> {
        self.parents[index].first().map(|&parent| parent as usize)
    }

    fn validate(&self) -> Result<(), String> {
        let mut seen = vec![false; self.order.len()];
        for &index in &self.order {
            match seen.get_mut(index as usize) {
                Some(seen) if !*seen => *seen = true,
                _ => return Err("compact header order is not a permutation".into()),
            }
        }
        let within_window = |index: usize, parent: u32| (1..PARENT_WINDOW).contains(&index.wrapping_sub(parent as usize));
        if self.parents.iter().enumerate().any(|(index, parents)| parents.len() > PARENTS || !parents.iter().all(|&parent| within_window(index, parent))) {
            return Err("compact header parents are not recent earlier points".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub rows: Axis,
    pub cols: Option<Axis>,
}

impl Layout {
    pub fn cols(&self) -> &Axis {
        self.cols.as_ref().unwrap_or(&self.rows)
    }

    pub fn frames(&self) -> usize {
        self.rows.order.len().div_ceil(FRAME_ROWS)
    }

    fn frame_rows(&self, frame: usize) -> Range<usize> {
        frame * FRAME_ROWS..((frame + 1) * FRAME_ROWS).min(self.rows.order.len())
    }
}

fn code_axis(coder: &mut impl Coder, axis: &Axis) -> Axis {
    let width = significant_bits(axis.order.len().saturating_sub(1) as u64);
    let order = axis.order.iter().map(|&index| coder.raw(index as u64, width) as u32).collect();
    let (mut counts, mut gaps) = ([Bit::UNKNOWN; 16], [Bit::UNKNOWN; 64]);
    let parents = axis
        .parents
        .iter()
        .enumerate()
        .map(|(index, parents)| {
            let count = coder.symbol(&mut counts, parents.len());
            let gap = |k: usize| parents.get(k).map_or(0, |&parent| (index - parent as usize) as u64);
            (0..count).map(|k| (index as u64).wrapping_sub(coder.magnitude(&mut gaps, gap(k))) as u32).collect()
        })
        .collect();
    Axis { order, parents }
}

pub fn encode_header(layout: &Layout) -> Vec<u8> {
    let mut encoder = Encoder::default();
    for axis in [Some(&layout.rows), layout.cols.as_ref()].into_iter().flatten() {
        code_axis(&mut encoder, axis);
    }
    let words = [layout.rows.order.len(), layout.cols().order.len(), layout.cols.is_none() as usize];
    MAGIC.into_iter().chain(words.into_iter().flat_map(|word| (word as u32).to_le_bytes())).chain(encoder.finish()).collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Exact,
    Residual,
    Unreachable,
}

const STATES: usize = 4;

fn context(left: Option<State>, above: Option<State>) -> usize {
    let index = |state: Option<State>| state.map_or(STATES - 1, |state| state as usize);
    index(left) * STATES + index(above)
}

#[derive(Clone, Copy)]
enum Cell {
    Exact,
    Unreachable,
    Residual { choice: usize, residual: [i64; 2] },
}

impl Cell {
    fn state(self) -> State {
        match self {
            Cell::Exact => State::Exact,
            Cell::Unreachable => State::Unreachable,
            Cell::Residual { .. } => State::Residual,
        }
    }
}

struct Model {
    exact: [Bit; STATES * STATES],
    unreachable: [Bit; STATES * STATES],
    choice: [Bit; PARENTS * PARENTS],
    zero: [Bit; 2],
    distance: [[Bit; 64]; 2],
    duration: [[Bit; 64]; 8],
}

impl Model {
    fn new() -> Self {
        Self {
            exact: [Bit::UNKNOWN; STATES * STATES],
            unreachable: [Bit::UNKNOWN; STATES * STATES],
            choice: [Bit::UNKNOWN; PARENTS * PARENTS],
            zero: [Bit::UNKNOWN; 2],
            distance: [[Bit::UNKNOWN; 64]; 2],
            duration: [[Bit::UNKNOWN; 64]; 8],
        }
    }

    fn code(&mut self, coder: &mut impl Coder, context: usize, cell: Cell) -> Cell {
        if !coder.bit(&mut self.exact[context], !matches!(cell, Cell::Exact)) {
            return Cell::Exact;
        }
        if coder.bit(&mut self.unreachable[context], matches!(cell, Cell::Unreachable)) {
            return Cell::Unreachable;
        }
        let (choice, [distance, duration]) = match cell {
            Cell::Residual { choice, residual } => (choice, residual),
            Cell::Exact | Cell::Unreachable => (0, [0, 0]),
        };
        let choice = coder.symbol(&mut self.choice, choice);
        if coder.bit(&mut self.zero[(choice != 0) as usize], distance == 0 && duration == 0) {
            return Cell::Residual { choice, residual: [0, 0] };
        }
        let distance = coder.magnitude(&mut self.distance[(choice != 0) as usize], zigzag(distance));
        let duration = coder.magnitude(&mut self.duration[(significant_bits(distance) as usize).min(28) / 4], zigzag(duration));
        Cell::Residual { choice, residual: [unzigzag(distance), unzigzag(duration)] }
    }
}

#[inline(always)]
fn predict(value: impl Fn(usize, usize) -> [i64; 2], row: usize, col: usize, above: Option<usize>, left: Option<usize>) -> [i64; 2] {
    match (above, left) {
        (Some(above), Some(left)) => {
            let ([a0, a1], [b0, b1], [c0, c1]) = (value(row, left), value(above, col), value(above, left));
            [a0 + b0 - c0, a1 + b1 - c1]
        }
        (Some(above), None) => value(above, col),
        (None, Some(left)) => value(row, left),
        (None, None) => [0, 0],
    }
}

fn candidate(above: &[u32], left: &[u32], choice: usize) -> Option<(Option<usize>, Option<usize>)> {
    let (a, b) = (choice / PARENTS, choice % PARENTS);
    let parent = |parents: &[u32], k: usize| parents.get(k).map(|&parent| parent as usize);
    (a < above.len().max(1) && b < left.len().max(1)).then(|| (parent(above, a), parent(left, b)))
}

fn candidates<'a>(above: &'a [u32], left: &'a [u32]) -> impl Iterator<Item = (usize, Option<usize>, Option<usize>)> + 'a {
    (0..PARENTS * PARENTS).filter_map(move |choice| candidate(above, left, choice).map(|(above, left)| (choice, above, left)))
}

pub fn encode_frame(layout: &Layout, frame: usize, cell: impl Fn(usize, usize) -> Option<[i64; 2]>) -> Vec<u8> {
    let (rows, cols) = (&layout.rows, layout.cols());
    let value = |row: usize, col: usize| cell(row, col).unwrap_or([0, 0]);
    let state = |row: usize, col: usize| match cell(row, col) {
        None => State::Unreachable,
        Some(actual) if actual == predict(value, row, col, rows.first_parent(row), cols.first_parent(col)) => State::Exact,
        Some(_) => State::Residual,
    };
    let mut encoder = Encoder::default();
    let mut model = Model::new();
    let mut row_states = vec![State::Unreachable; cols.order.len()];
    for row in layout.frame_rows(frame) {
        let above = rows.first_parent(row);
        for col in 0..cols.order.len() {
            let left = cols.first_parent(col);
            let context = context(left.map(|left| row_states[left]), above.map(|above| state(above, col)));
            let code = match cell(row, col) {
                None => Cell::Unreachable,
                Some(actual) if actual == predict(value, row, col, above, left) => Cell::Exact,
                Some(actual) => {
                    let mut best = (u32::MAX, Cell::Exact);
                    for (choice, above, left) in candidates(&rows.parents[row], &cols.parents[col]) {
                        let [distance, duration] = predict(value, row, col, above, left);
                        let residual = [actual[0] - distance, actual[1] - duration];
                        let cost = significant_bits(zigzag(residual[0])) + significant_bits(zigzag(residual[1]));
                        if cost < best.0 {
                            best = (cost, Cell::Residual { choice, residual });
                        }
                        if cost == 0 {
                            break;
                        }
                    }
                    best.1
                }
            };
            row_states[col] = code.state();
            model.code(&mut encoder, context, code);
        }
    }
    encoder.finish()
}

#[derive(Default)]
enum Stage {
    #[default]
    Header,
    Frames,
    Trailer,
    Done(ServerTimes),
}

#[derive(Default)]
pub struct CompactDecoder {
    pending: Vec<u8>,
    stage: Stage,
    layout: Layout,
    left_parents: Vec<Option<usize>>,
    states: Vec<State>,
    values: Vec<[i64; 2]>,
    row_states: Vec<State>,
    row_values: Vec<[i64; 2]>,
    distances: Vec<u32>,
    durations: Vec<u32>,
    decoded_frames: usize,
}

impl CompactDecoder {
    pub fn feed(&mut self, bytes: &[u8], sink: &mut impl RowSink) -> Result<(), String> {
        let mut pending = std::mem::take(&mut self.pending);
        pending.extend_from_slice(bytes);
        let mut offset = 0;
        loop {
            let available = &pending[offset..];
            match self.stage {
                Stage::Header => {
                    if available.len() < HEADER_LEN + SECTION_HEADER_LEN {
                        break;
                    }
                    if available[..4] != MAGIC {
                        return Err("not a compact distance matrix body".into());
                    }
                    let end = HEADER_LEN + section_len(&available[HEADER_LEN..]);
                    if available.len() < end {
                        break;
                    }
                    let [rows, cols, shared] = words(&available[4..HEADER_LEN])[..] else { unreachable!("three words") };
                    self.start(rows as usize, cols as usize, shared == 1, &available[HEADER_LEN..end])?;
                    sink.start(rows as usize, cols as usize);
                    offset += end;
                    self.stage = if rows == 0 { Stage::Trailer } else { Stage::Frames };
                }
                Stage::Frames => {
                    if available.len() < SECTION_HEADER_LEN || available.len() < section_len(available) {
                        break;
                    }
                    let end = section_len(available);
                    self.decode_frame(&available[..end], sink)?;
                    offset += end;
                    self.decoded_frames += 1;
                    if self.decoded_frames == self.layout.frames() {
                        self.stage = Stage::Trailer;
                    }
                }
                Stage::Trailer => {
                    if available.len() < TRAILER_LEN {
                        break;
                    }
                    let [queue_us, prepare_us, compute_us] = words(&available[..TRAILER_LEN])[..] else { unreachable!("three words") };
                    offset += TRAILER_LEN;
                    self.stage = Stage::Done(ServerTimes { queue_us, prepare_us, compute_us });
                }
                Stage::Done(_) if available.is_empty() => break,
                Stage::Done(_) => return Err("unexpected bytes after the matrix".into()),
            }
        }
        self.pending = pending[offset..].to_vec();
        Ok(())
    }

    fn start(&mut self, rows: usize, cols: usize, shared: bool, section: &[u8]) -> Result<(), String> {
        if shared && rows != cols {
            return Err("compact header shares axes of different lengths".into());
        }
        let mut decoder = Decoder::new(section);
        let mut axis = |len: usize| code_axis(&mut decoder, &Axis { order: vec![0; len], parents: vec![Vec::new(); len] });
        let rows_axis = axis(rows);
        self.layout = Layout { rows: rows_axis, cols: (!shared).then(|| axis(cols)) };
        self.layout.rows.validate()?;
        self.layout.cols().validate()?;
        self.left_parents = (0..cols).map(|col| self.layout.cols().first_parent(col)).collect();
        let kept = rows.min(PARENT_WINDOW) * cols;
        (self.states, self.values) = (vec![State::Unreachable; kept], vec![[0, 0]; kept]);
        (self.row_states, self.row_values) = (vec![State::Unreachable; cols], vec![[0, 0]; cols]);
        (self.distances, self.durations) = (vec![NO_ROUTE; cols], vec![NO_ROUTE; cols]);
        Ok(())
    }

    fn decode_frame(&mut self, section: &[u8], sink: &mut impl RowSink) -> Result<(), String> {
        let Self { layout, left_parents, states, values, row_states, row_values, distances, durations, decoded_frames, .. } = self;
        let (rows, cols) = (&layout.rows, layout.cols());
        let width = cols.order.len();
        let slot = |row: usize| (row % PARENT_WINDOW) * width;
        let mut decoder = Decoder::new(section);
        let mut model = Model::new();
        for row in layout.frame_rows(*decoded_frames) {
            let above = rows.first_parent(row);
            for (col, &left) in left_parents.iter().enumerate() {
                let context = context(left.map(|left| row_states[left]), above.map(|above| states[slot(above) + col]));
                let cell = model.code(&mut decoder, context, Cell::Exact);
                let at = |other: usize, col: usize| if other == row { row_values[col] } else { values[slot(other) + col] };
                let value = match cell {
                    Cell::Exact => predict(at, row, col, above, left),
                    Cell::Unreachable => [0, 0],
                    Cell::Residual { choice, residual } => {
                        let (above, left) = candidate(&rows.parents[row], &cols.parents[col], choice).ok_or("compact frame refers to a missing parent")?;
                        let [distance, duration] = predict(at, row, col, above, left);
                        [distance + residual[0], duration + residual[1]]
                    }
                };
                if !value.iter().all(|value| (0..VALUE_LIMIT).contains(value)) {
                    return Err("compact frame decodes to an impossible value".into());
                }
                (row_states[col], row_values[col]) = (cell.state(), value);
                let output = cols.order[col] as usize;
                (distances[output], durations[output]) =
                    if cell.state() == State::Unreachable { (NO_ROUTE, NO_ROUTE) } else { (meters(value[0] as u64), seconds(value[1] as u64)) };
            }
            states[slot(row)..slot(row) + width].copy_from_slice(row_states);
            values[slot(row)..slot(row) + width].copy_from_slice(row_values);
            sink.row(rows.order[row] as usize, distances, durations);
        }
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        matches!(self.stage, Stage::Done(_))
    }

    pub fn finish(self) -> Result<ServerTimes, String> {
        match self.stage {
            Stage::Done(times) => Ok(times),
            _ => Err("compact body is truncated".into()),
        }
    }
}

pub fn decode(body: &[u8]) -> Result<(DecodedMatrix, ServerTimes), String> {
    let (mut decoder, mut matrix) = (CompactDecoder::default(), DecodedMatrix::default());
    decoder.feed(body, &mut matrix)?;
    Ok((matrix, decoder.finish()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Random(u64);

    impl Random {
        fn below(&mut self, limit: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % limit
        }
    }

    fn axis(len: usize, random: &mut Random) -> Axis {
        let mut order: Vec<u32> = (0..len as u32).collect();
        for index in (1..len).rev() {
            order.swap(index, random.below(index as u64 + 1) as usize);
        }
        let parents = (0..len).map(|index| (0..index.min(PARENTS)).map(|_| random.below(index as u64) as u32).collect()).collect();
        Axis { order, parents }
    }

    fn body(layout: &Layout, cell: impl Fn(usize, usize) -> Option<[i64; 2]>, times: ServerTimes) -> Vec<u8> {
        let frames = (0..layout.frames()).flat_map(|frame| encode_frame(layout, frame, &cell));
        encode_header(layout).into_iter().chain(frames).chain(times.to_bytes()).collect()
    }

    #[test]
    fn round_trips_hub_structured_matrices_with_holes_and_unsnapped_points() {
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        let times = ServerTimes { queue_us: 1, prepare_us: 20, compute_us: 300 };
        for (rows, cols, shared) in [(0, 0, true), (1, 1, true), (5, 3, false), (40, 40, true), (70, 45, false), (33, 1, false)] {
            let row_axis = axis(rows, &mut random);
            let layout = Layout { cols: (!shared).then(|| axis(cols, &mut random)), rows: row_axis };
            let hubs: Vec<[i64; 4]> = (0..rows.max(cols)).map(|_| [0; 4].map(|_| random.below(40_000_000) as i64)).collect();
            let unsnapped = |index: usize| index % 17 == 11;
            let holes: Vec<bool> = (0..rows * cols).map(|_| random.below(50) == 0).collect();
            let cell = |row: usize, col: usize| {
                let reachable = !unsnapped(row) && !unsnapped(col) && !holes[row * cols + col];
                let via = |hub: usize| hubs[row][hub] + hubs[col][3 - hub];
                let distance = via(0).min(via(1)) + (row == col) as i64 * 7;
                reachable.then_some([distance, distance * 36 + (row * col % 5) as i64 * 999])
            };
            let body = body(&layout, cell, times);
            let (mut streamed, mut decoded) = (CompactDecoder::default(), DecodedMatrix::default());
            for piece in body.chunks(7) {
                streamed.feed(piece, &mut decoded).unwrap();
            }
            assert_eq!(streamed.finish().unwrap(), times);
            assert_eq!(decode(&body).unwrap().0, decoded);
            for row in 0..rows {
                for col in 0..cols {
                    let target = layout.rows.order[row] as usize * cols + layout.cols().order[col] as usize;
                    let expected = cell(row, col).map_or((NO_ROUTE, NO_ROUTE), |[distance, duration]| (meters(distance as u64), seconds(duration as u64)));
                    assert_eq!((decoded.distances[target], decoded.durations[target]), expected, "{rows}x{cols} at {row},{col}");
                }
            }
        }
    }

    #[test]
    fn rejects_bodies_that_do_not_fit_their_header() {
        let layout = Layout { rows: Axis { order: vec![1, 0], parents: vec![vec![], vec![0]] }, cols: None };
        let body = body(&layout, |row, col| Some([(row + col) as i64, 1]), ServerTimes { queue_us: 0, prepare_us: 0, compute_us: 0 });
        assert!(decode(&body[..body.len() - 1]).is_err());
        assert!(decode(&[body.as_slice(), &[0]].concat()).is_err());
        let broken_parents = Layout { rows: Axis { order: vec![1, 0], parents: vec![vec![], vec![1]] }, cols: None };
        assert!(decode(&encode_header(&broken_parents)).is_err());
        let rows = PARENT_WINDOW + 1;
        let beyond_window =
            Layout { rows: Axis { order: (0..rows as u32).collect(), parents: (0..rows).map(|index| vec![0; (index > 0) as usize]).collect() }, cols: None };
        assert!(decode(&encode_header(&beyond_window)).is_err());
    }

    #[test]
    fn parents_reach_back_across_the_whole_window() {
        let rows = PARENT_WINDOW + 300;
        let parents = (0..rows)
            .map(|index| [index.saturating_sub(PARENT_WINDOW - 1), index.saturating_sub(1)].into_iter().filter(|&p| p < index).map(|p| p as u32).collect())
            .collect();
        let layout = Layout {
            rows: Axis { order: (0..rows as u32).rev().collect(), parents },
            cols: Some(Axis { order: vec![1, 2, 0], parents: vec![vec![], vec![0], vec![1, 0]] }),
        };
        let cell = |row: usize, col: usize| (row % 97 != 5).then_some([(row * 1_000 + col * 7) as i64, (row * 36_000 + col * 250) as i64 + (row % 3) as i64]);
        let (matrix, _) = decode(&body(&layout, cell, ServerTimes { queue_us: 0, prepare_us: 0, compute_us: 0 })).unwrap();
        for row in 0..rows {
            for col in 0..3 {
                let target = (rows - 1 - row) * 3 + layout.cols().order[col] as usize;
                let expected = cell(row, col).map_or((NO_ROUTE, NO_ROUTE), |[distance, duration]| (meters(distance as u64), seconds(duration as u64)));
                assert_eq!((matrix.distances[target], matrix.durations[target]), expected, "{row},{col}");
            }
        }
    }
}
