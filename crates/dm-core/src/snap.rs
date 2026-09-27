use crate::geo::{project_origin_onto_segment, Coord, LocalFrame, Point};
use crate::network::Network;

#[derive(Clone, Copy, Debug)]
pub struct SnapConfig {
    pub max_distance_m: f64,
    pub minor_component_margin_m: f64,
}

impl Default for SnapConfig {
    fn default() -> Self {
        Self { max_distance_m: 5_000.0, minor_component_margin_m: 1_000.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub chain: u32,
    pub fraction: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snap {
    pub placement: Placement,
    pub distance_m: f64,
}

#[derive(Clone, Copy)]
struct SegmentHit {
    segment: usize,
    t: f64,
}

fn closest_segment(network: &Network, frame: &LocalFrame, chain: u32) -> (f64, SegmentHit) {
    let mut points = network.chains.polyline(chain, &network.node_coords).map(|c| frame.project(c));
    let mut previous = points.next().expect("chains have two endpoints");
    let mut best = (f64::INFINITY, SegmentHit { segment: 0, t: 0.0 });
    for (segment, current) in points.enumerate() {
        let hit = project_origin_onto_segment(previous, current);
        if hit.distance_m < best.0 {
            best = (hit.distance_m, SegmentHit { segment, t: hit.t });
        }
        previous = current;
    }
    best
}

fn fraction_along(network: &Network, frame: &LocalFrame, chain: u32, hit: SegmentHit) -> f64 {
    let points: Vec<Point> = network.chains.polyline(chain, &network.node_coords).map(|c| frame.project(c)).collect();
    let lengths: Vec<f64> = points.windows(2).map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y)).collect();
    let total: f64 = lengths.iter().sum();
    if total == 0.0 {
        return 0.0;
    }
    let along: f64 = lengths[..hit.segment].iter().sum::<f64>() + hit.t * lengths[hit.segment];
    (along / total).clamp(0.0, 1.0)
}

pub fn snap(network: &Network, coord: Coord, config: &SnapConfig) -> Option<Snap> {
    let frame = LocalFrame::new(coord);
    let exact = |chain: u32| Some(closest_segment(network, &frame, chain));
    let major = network.major_index.nearest(&frame, config.max_distance_m, exact);
    let minor_radius = major.as_ref().map_or(config.max_distance_m, |m| m.distance_m - config.minor_component_margin_m);
    let minor = if minor_radius > 0.0 { network.minor_index.nearest(&frame, minor_radius, exact) } else { None };
    let chosen = minor.or(major)?;
    Some(Snap {
        placement: Placement { chain: chosen.item, fraction: fraction_along(network, &frame, chosen.item, chosen.hit) },
        distance_m: chosen.distance_m,
    })
}
