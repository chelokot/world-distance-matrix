use bytemuck::{Pod, Zeroable};

pub const COORD_SCALE: f64 = 1e7;
const EARTH_RADIUS_M: f64 = 6_371_008.8;
const METRES_PER_DEGREE: f64 = EARTH_RADIUS_M * std::f64::consts::PI / 180.0;
const FULL_TURN: i64 = 3_600_000_000;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct Coord {
    pub lat: i32,
    pub lon: i32,
}

impl Coord {
    pub fn from_degrees(lat: f64, lon: f64) -> Self {
        let lon = ((lon * COORD_SCALE).round() as i64 + FULL_TURN / 2).rem_euclid(FULL_TURN) - FULL_TURN / 2;
        Self { lat: (lat * COORD_SCALE).round() as i32, lon: lon as i32 }
    }

    pub fn lat_degrees(self) -> f64 {
        self.lat as f64 / COORD_SCALE
    }

    pub fn lon_degrees(self) -> f64 {
        self.lon as f64 / COORD_SCALE
    }
}

pub fn haversine_m(a: Coord, b: Coord) -> f64 {
    let lat_a = a.lat_degrees().to_radians();
    let lat_b = b.lat_degrees().to_radians();
    let half_dlat = (lat_b - lat_a) / 2.0;
    let half_dlon = (b.lon_degrees() - a.lon_degrees()).to_radians() / 2.0;
    let h = half_dlat.sin().powi(2) + lat_a.cos() * lat_b.cos() * half_dlon.sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub struct LocalFrame {
    origin: Coord,
    metres_per_lon_unit: f64,
}

const METRES_PER_UNIT: f64 = METRES_PER_DEGREE / COORD_SCALE;

impl LocalFrame {
    pub fn new(origin: Coord) -> Self {
        Self { origin, metres_per_lon_unit: METRES_PER_UNIT * origin.lat_degrees().to_radians().cos() }
    }

    fn lon_offset(&self, lon: i32) -> i64 {
        (lon as i64 - self.origin.lon as i64 + FULL_TURN / 2).rem_euclid(FULL_TURN) - FULL_TURN / 2
    }

    pub fn project(&self, coord: Coord) -> Point {
        Point { x: self.lon_offset(coord.lon) as f64 * self.metres_per_lon_unit, y: (coord.lat as i64 - self.origin.lat as i64) as f64 * METRES_PER_UNIT }
    }

    pub fn distance_to_box_m(&self, min: Coord, max: Coord) -> f64 {
        let lon = if (min.lon..=max.lon).contains(&self.origin.lon) {
            self.origin.lon
        } else if self.lon_offset(min.lon).abs() <= self.lon_offset(max.lon).abs() {
            min.lon
        } else {
            max.lon
        };
        let p = self.project(Coord { lat: self.origin.lat.clamp(min.lat, max.lat), lon });
        p.x.hypot(p.y)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SegmentProjection {
    pub distance_m: f64,
    pub t: f64,
}

pub fn project_origin_onto_segment(a: Point, b: Point) -> SegmentProjection {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length_sq = dx * dx + dy * dy;
    let t = if length_sq > 0.0 { (-(a.x * dx + a.y * dy) / length_sq).clamp(0.0, 1.0) } else { 0.0 };
    SegmentProjection { distance_m: (a.x + t * dx).hypot(a.y + t * dy), t }
}

pub fn hilbert_index(coord: Coord) -> u64 {
    const ORDER: u32 = 31;
    let side = 1u64 << ORDER;
    let x = (((coord.lon as i64 + 1_800_000_000) as u128 * side as u128) / 3_600_000_001) as u64;
    let y = (((coord.lat as i64 + 900_000_000) as u128 * side as u128) / 1_800_000_001) as u64;
    let (mut x, mut y) = (x, y);
    let mut index = 0u64;
    let mut s = side >> 1;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        index += s * s * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = side - 1 - x;
                y = side - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_matches_known_distance() {
        let berlin = Coord::from_degrees(52.5200, 13.4050);
        let hamburg = Coord::from_degrees(53.5511, 9.9937);
        let d = haversine_m(berlin, hamburg);
        assert!((d - 255_000.0).abs() < 2_000.0, "{d}");
    }

    #[test]
    fn local_frame_agrees_with_haversine_at_short_range() {
        let origin = Coord::from_degrees(54.32, 10.13);
        let other = Coord::from_degrees(54.33, 10.15);
        let p = LocalFrame::new(origin).project(other);
        let exact = haversine_m(origin, other);
        assert!((p.x.hypot(p.y) - exact).abs() / exact < 0.002);
    }

    #[test]
    fn local_frame_wraps_antimeridian() {
        let frame = LocalFrame::new(Coord::from_degrees(-17.0, 179.999));
        let p = frame.project(Coord::from_degrees(-17.0, -179.999));
        assert!(p.x > 0.0 && p.x < 300.0, "{}", p.x);
        assert_eq!(Coord::from_degrees(-17.0, 180.0), Coord::from_degrees(-17.0, -180.0));
        let box_across = frame.distance_to_box_m(Coord::from_degrees(-17.1, -179.99), Coord::from_degrees(-16.9, -179.5));
        assert!(box_across < 1_200.0, "{box_across}");
    }

    #[test]
    fn segment_projection_clamps_to_endpoints() {
        let hit = project_origin_onto_segment(Point { x: -1.0, y: 1.0 }, Point { x: 1.0, y: 1.0 });
        assert!((hit.t - 0.5).abs() < 1e-12 && (hit.distance_m - 1.0).abs() < 1e-12);
        let end = project_origin_onto_segment(Point { x: 1.0, y: 1.0 }, Point { x: 2.0, y: 1.0 });
        assert_eq!(end.t, 0.0);
        let degenerate = project_origin_onto_segment(Point { x: 3.0, y: 4.0 }, Point { x: 3.0, y: 4.0 });
        assert_eq!(degenerate.distance_m, 5.0);
    }

    #[test]
    fn hilbert_keeps_neighbours_close() {
        let a = hilbert_index(Coord::from_degrees(54.0, 10.0));
        let b = hilbert_index(Coord::from_degrees(54.0000001, 10.0000001));
        let far = hilbert_index(Coord::from_degrees(-33.0, 151.0));
        assert!(a.abs_diff(b) < a.abs_diff(far));
    }
}
