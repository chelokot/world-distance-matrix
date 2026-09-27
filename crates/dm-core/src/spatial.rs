use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bytemuck::{Pod, Zeroable};

use crate::geo::{Coord, LocalFrame};
use crate::store::Array;

pub const FANOUT: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct BBox {
    pub min: Coord,
    pub max: Coord,
}

impl BBox {
    pub const EMPTY: BBox = BBox { min: Coord { lat: i32::MAX, lon: i32::MAX }, max: Coord { lat: i32::MIN, lon: i32::MIN } };

    pub fn include(&mut self, coord: Coord) {
        self.min.lat = self.min.lat.min(coord.lat);
        self.min.lon = self.min.lon.min(coord.lon);
        self.max.lat = self.max.lat.max(coord.lat);
        self.max.lon = self.max.lon.max(coord.lon);
    }

    pub fn union(mut self, other: &BBox) -> BBox {
        self.include(other.min);
        self.include(other.max);
        self
    }

    pub fn center(&self) -> Coord {
        Coord { lat: ((self.min.lat as i64 + self.max.lat as i64) / 2) as i32, lon: ((self.min.lon as i64 + self.max.lon as i64) / 2) as i32 }
    }
}

fn level_sizes(item_count: usize) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut size = item_count.div_ceil(FANOUT);
    while size > 0 {
        sizes.push(size);
        if size == 1 {
            break;
        }
        size = size.div_ceil(FANOUT);
    }
    sizes
}

pub fn build_levels(item_boxes: &[BBox]) -> Vec<BBox> {
    let mut boxes: Vec<BBox> = item_boxes.chunks(FANOUT).map(|chunk| chunk.iter().fold(BBox::EMPTY, |acc, b| acc.union(b))).collect();
    let mut level_start = 0;
    while boxes.len() - level_start > 1 {
        let level_end = boxes.len();
        let parents: Vec<BBox> = boxes[level_start..level_end].chunks(FANOUT).map(|chunk| chunk.iter().fold(BBox::EMPTY, |acc, b| acc.union(b))).collect();
        boxes.extend(parents);
        level_start = level_end;
    }
    boxes
}

pub struct PackedRtree {
    first_item: u32,
    item_count: u32,
    boxes: Array<BBox>,
    level_starts: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
pub struct Nearest<H> {
    pub item: u32,
    pub distance_m: f64,
    pub hit: H,
}

impl PackedRtree {
    pub fn new(first_item: u32, item_count: u32, boxes: Array<BBox>) -> Self {
        let sizes = level_sizes(item_count as usize);
        assert_eq!(boxes.len(), sizes.iter().sum::<usize>(), "spatial index does not match its item count");
        let level_starts = sizes.iter().scan(0, |start, size| {
            let current = *start;
            *start += size;
            Some(current)
        });
        Self { first_item, item_count, level_starts: level_starts.collect(), boxes }
    }

    pub fn nearest<H>(&self, frame: &LocalFrame, max_distance_m: f64, mut exact: impl FnMut(u32) -> Option<(f64, H)>) -> Option<Nearest<H>> {
        let top = self.level_starts.len().checked_sub(1)?;
        let mut best: Option<Nearest<H>> = None;
        let mut radius = max_distance_m;
        let mut queue = BinaryHeap::new();
        let root = self.boxes[self.level_starts[top]];
        queue.push(Reverse((frame.distance_to_box_m(root.min, root.max).to_bits(), top, 0usize)));
        while let Some(Reverse((bound_bits, level, index))) = queue.pop() {
            if f64::from_bits(bound_bits) > radius {
                break;
            }
            let children = index * FANOUT..(index + 1) * FANOUT;
            if level == 0 {
                let first = self.first_item as usize;
                for item in children.start..children.end.min(self.item_count as usize) {
                    let item = (first + item) as u32;
                    if let Some((distance_m, hit)) = exact(item) {
                        if distance_m <= radius && best.as_ref().is_none_or(|b| distance_m < b.distance_m) {
                            radius = distance_m;
                            best = Some(Nearest { item, distance_m, hit });
                        }
                    }
                }
            } else {
                let child_level_start = self.level_starts[level - 1];
                let child_level_len = self.level_starts[level] - child_level_start;
                for child in children.start..children.end.min(child_level_len) {
                    let b = self.boxes[child_level_start + child];
                    let bound = frame.distance_to_box_m(b.min, b.max);
                    if bound <= radius {
                        queue.push(Reverse((bound.to_bits(), level - 1, child)));
                    }
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::haversine_m;
    use rand::{Rng, SeedableRng};

    #[test]
    fn nearest_matches_brute_force_including_across_the_antimeridian() {
        for (lat, lon) in [(54.0, 10.0), (-16.8, 180.0)] {
            nearest_matches_brute_force_around(lat, lon);
        }
    }

    fn nearest_matches_brute_force_around(lat: f64, lon: f64) {
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let mut points: Vec<Coord> = (0..5000).map(|_| Coord::from_degrees(lat + rng.gen_range(-0.5..0.5), lon + rng.gen_range(-0.8..0.8))).collect();
        points.sort_by_key(|&p| crate::geo::hilbert_index(p));
        let boxes: Vec<BBox> = points.iter().map(|&p| BBox { min: p, max: p }).collect();
        let tree = PackedRtree::new(0, points.len() as u32, build_levels(&boxes).into());
        for _ in 0..200 {
            let query = Coord::from_degrees(lat + rng.gen_range(-0.6..0.6), lon + rng.gen_range(-0.9..0.9));
            let frame = LocalFrame::new(query);
            let found = tree
                .nearest(&frame, 1e7, |item| {
                    let p = frame.project(points[item as usize]);
                    Some((p.x.hypot(p.y), ()))
                })
                .unwrap();
            let brute = points
                .iter()
                .map(|&p| {
                    let q = frame.project(p);
                    q.x.hypot(q.y)
                })
                .fold(f64::INFINITY, f64::min);
            assert!((found.distance_m - brute).abs() < 1e-9);
            assert!((haversine_m(query, points[found.item as usize]) - brute).abs() / brute.max(1.0) < 0.01);
        }
    }

    #[test]
    fn box_distance_never_exceeds_the_distance_to_a_point_inside() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(11);
        for _ in 0..20_000 {
            let corner = |rng: &mut rand::rngs::StdRng| Coord::from_degrees(rng.gen_range(-60.0..60.0), 180.0 + rng.gen_range(-3.0..3.0));
            let (a, b) = (corner(&mut rng), corner(&mut rng));
            let mut bbox = BBox::EMPTY;
            bbox.include(a);
            bbox.include(b);
            let inside = Coord { lat: rng.gen_range(bbox.min.lat..=bbox.max.lat), lon: rng.gen_range(bbox.min.lon..=bbox.max.lon) };
            let frame = LocalFrame::new(corner(&mut rng));
            let p = frame.project(inside);
            assert!(frame.distance_to_box_m(bbox.min, bbox.max) <= p.x.hypot(p.y) + 1e-6);
        }
    }

    #[test]
    fn radius_limits_search() {
        let points = [Coord::from_degrees(0.0, 0.0), Coord::from_degrees(0.0, 0.01)];
        let boxes: Vec<BBox> = points.iter().map(|&p| BBox { min: p, max: p }).collect();
        let tree = PackedRtree::new(0, 2, build_levels(&boxes).into());
        let frame = LocalFrame::new(Coord::from_degrees(0.0, 0.005));
        let found = tree.nearest(&frame, 100.0, |item| {
            let p = frame.project(points[item as usize]);
            Some((p.x.hypot(p.y), ()))
        });
        assert!(found.is_none());
    }

    #[test]
    fn empty_tree_finds_nothing() {
        let tree = PackedRtree::new(0, 0, Vec::new().into());
        assert!(tree.nearest(&LocalFrame::new(Coord::from_degrees(0.0, 0.0)), 1e9, |_| Some((0.0, ()))).is_none());
    }
}
