use dm_core::geo::{haversine_m, project_origin_onto_segment, Coord, LocalFrame, Point};
use dm_core::weight::{ChainCost, NOT_TRAVERSABLE};
use rayon::prelude::*;

use crate::osm::{OsmExtract, NODE_BLOCKS_CARS, NODE_PRESENT, NODE_TRAFFIC_SIGNAL};
use crate::profile::{TurnRule, WayProfile, TRAFFIC_SIGNAL_PENALTY_MS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chain {
    pub tail: u32,
    pub head: u32,
    pub cost: ChainCost,
}

pub struct GraphRestriction {
    pub rule: TurnRule,
    pub from: Vec<u32>,
    pub via: u32,
    pub to: Vec<u32>,
}

pub struct Topology {
    pub node_coords: Vec<Coord>,
    pub chains: Vec<Chain>,
    pub chain_way: Vec<u32>,
    pub chain_snappable: Vec<bool>,
    pub interior_first: Vec<u64>,
    pub interior: Vec<Coord>,
    pub restrictions: Vec<GraphRestriction>,
}

impl Topology {
    pub fn node_count(&self) -> usize {
        self.node_coords.len()
    }

    pub fn simplify_geometry(&mut self, tolerance_m: f64) {
        let chunks: Vec<(Vec<u32>, Vec<Coord>)> = (0..self.chains.len())
            .into_par_iter()
            .chunks(1 << 16)
            .map(|chains| {
                let mut counts = Vec::with_capacity(chains.len());
                let mut kept_interior = Vec::new();
                for chain in chains {
                    let c = &self.chains[chain];
                    let full: Vec<Coord> = std::iter::once(self.node_coords[c.tail as usize])
                        .chain(self.interior(chain).iter().copied())
                        .chain(std::iter::once(self.node_coords[c.head as usize]))
                        .collect();
                    let kept = simplify(&full, tolerance_m);
                    counts.push((kept.len() - 2) as u32);
                    kept_interior.extend_from_slice(&kept[1..kept.len() - 1]);
                }
                (counts, kept_interior)
            })
            .collect();
        self.interior = Vec::with_capacity(chunks.iter().map(|(_, coords)| coords.len()).sum());
        self.interior_first.clear();
        self.interior_first.push(0);
        for (counts, coords) in chunks {
            for count in counts {
                self.interior_first.push(self.interior_first.last().expect("starts with zero") + count as u64);
            }
            self.interior.extend(coords);
        }
    }

    pub fn interior(&self, chain: usize) -> &[Coord] {
        &self.interior[self.interior_first[chain] as usize..self.interior_first[chain + 1] as usize]
    }
}

fn simplify(points: &[Coord], tolerance_m: f64) -> Vec<Coord> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let frame = LocalFrame::new(points[0]);
    let projected: Vec<Point> = points.iter().map(|&c| frame.project(c)).collect();
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut pending = vec![(0usize, points.len() - 1)];
    while let Some((first, last)) = pending.pop() {
        let (a, b) = (projected[first], projected[last]);
        let farthest = (first + 1..last)
            .map(|i| {
                let p = projected[i];
                let hit = project_origin_onto_segment(Point { x: a.x - p.x, y: a.y - p.y }, Point { x: b.x - p.x, y: b.y - p.y });
                (hit.distance_m, i)
            })
            .max_by(|x, y| x.0.total_cmp(&y.0));
        if let Some((distance, index)) = farthest {
            if distance > tolerance_m {
                keep[index] = true;
                pending.push((first, index));
                pending.push((index, last));
            }
        }
    }
    points.iter().zip(keep).filter_map(|(&c, k)| k.then_some(c)).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum End {
    Junction(u32),
    Fresh(u32),
}

#[derive(Default)]
struct ChunkChains {
    chains: Vec<(End, End, ChainCost, u32, u32)>,
    interior: Vec<Coord>,
    fresh_coords: Vec<Coord>,
}

struct OpenChain {
    start: End,
    start_coord: Coord,
    start_signal: bool,
    length_m: f64,
    signals: u32,
    interior_start: usize,
}

struct Context<'a> {
    extract: &'a OsmExtract,
    junction_id: &'a [u32],
}

struct WayCosts<'a> {
    way: u32,
    profile: &'a WayProfile,
    speeds: (f64, f64),
}

const NOT_JUNCTION: u32 = u32::MAX;

fn polyline_length_m(points: &[Coord]) -> f64 {
    points.windows(2).map(|pair| haversine_m(pair[0], pair[1])).sum()
}

fn directed_time_ms(length_m: f64, speed_kmh: f64, signals: u32) -> u32 {
    ((length_m * 3600.0 / speed_kmh).round() as u32).max(1) + signals * TRAFFIC_SIGNAL_PENALTY_MS
}

impl ChunkChains {
    fn end_at(&mut self, ctx: &Context, node: u32) -> End {
        if ctx.extract.nodes.flags[node as usize] & NODE_BLOCKS_CARS != 0 {
            self.fresh_coords.push(ctx.extract.nodes.coords[node as usize]);
            End::Fresh(self.fresh_coords.len() as u32 - 1)
        } else {
            End::Junction(ctx.junction_id[node as usize])
        }
    }

    fn push(&mut self, costs: &WayCosts, tail: End, head: End, length_m: f64, signals: (u32, u32), interior_len: usize) {
        let cost = ChainCost {
            dist_dm: ((length_m * 10.0).round() as u32).max(1),
            forward_time_ms: if costs.profile.forward { directed_time_ms(length_m, costs.speeds.0, signals.0) } else { NOT_TRAVERSABLE },
            backward_time_ms: if costs.profile.backward { directed_time_ms(length_m, costs.speeds.1, signals.1) } else { NOT_TRAVERSABLE },
        };
        self.chains.push((tail, head, cost, interior_len as u32, costs.way));
    }

    fn close(&mut self, costs: &WayCosts, open: OpenChain, end: End, end_coord: Coord, end_signal: bool) {
        let interior_len = self.interior.len() - open.interior_start;
        let end_signals = (open.signals + end_signal as u32, open.signals + open.start_signal as u32);
        if open.start != end {
            self.push(costs, open.start, end, open.length_m, end_signals, interior_len);
            return;
        }
        if interior_len == 0 {
            return;
        }
        let middle_index = open.interior_start + interior_len / 2;
        let middle = self.interior.remove(middle_index);
        let first: Vec<Coord> =
            std::iter::once(open.start_coord).chain(self.interior[open.interior_start..middle_index].iter().copied()).chain([middle]).collect();
        let second: Vec<Coord> = std::iter::once(middle).chain(self.interior[middle_index..].iter().copied()).chain([end_coord]).collect();
        self.fresh_coords.push(middle);
        let middle_end = End::Fresh(self.fresh_coords.len() as u32 - 1);
        let first_signals = (open.signals, open.signals + open.start_signal as u32);
        self.push(costs, open.start, middle_end, polyline_length_m(&first), first_signals, first.len() - 2);
        self.push(costs, middle_end, end, polyline_length_m(&second), (end_signal as u32, 0), second.len() - 2);
    }

    fn walk(&mut self, ctx: &Context, way: usize) {
        let nodes = &ctx.extract.nodes;
        let refs = ctx.extract.ways.refs(way);
        let profile = &ctx.extract.ways.profiles[way];
        let present = |node: u32| nodes.flags[node as usize] & NODE_PRESENT != 0;
        let way_length_m: f64 = refs
            .windows(2)
            .filter(|pair| present(pair[0]) && present(pair[1]))
            .map(|pair| haversine_m(nodes.coords[pair[0] as usize], nodes.coords[pair[1] as usize]))
            .sum();
        let costs = WayCosts { way: way as u32, profile, speeds: profile.speeds_kmh(way_length_m) };
        let mut open: Option<OpenChain> = None;
        let mut previous = Coord { lat: 0, lon: 0 };
        for &node in refs {
            if !present(node) {
                if let Some(abandoned) = open.take() {
                    self.interior.truncate(abandoned.interior_start);
                }
                continue;
            }
            let coord = nodes.coords[node as usize];
            let signal = nodes.flags[node as usize] & NODE_TRAFFIC_SIGNAL != 0;
            if let Some(chain) = open.as_mut() {
                chain.length_m += haversine_m(previous, coord);
            }
            previous = coord;
            let splits = ctx.junction_id[node as usize] != NOT_JUNCTION || nodes.flags[node as usize] & NODE_BLOCKS_CARS != 0;
            if splits {
                if let Some(chain) = open.take() {
                    let end = self.end_at(ctx, node);
                    self.close(&costs, chain, end, coord, signal);
                }
                let start = self.end_at(ctx, node);
                open = Some(OpenChain { start, start_coord: coord, start_signal: signal, length_m: 0.0, signals: 0, interior_start: self.interior.len() });
            } else if let Some(chain) = open.as_mut() {
                self.interior.push(coord);
                chain.signals += signal as u32;
            }
        }
        if let Some(abandoned) = open {
            self.interior.truncate(abandoned.interior_start);
        }
    }
}

fn junction_ids(extract: &OsmExtract) -> (Vec<u32>, usize) {
    let node_count = extract.nodes.coords.len();
    let mut uses = vec![0u8; node_count];
    let present = |node: u32| extract.nodes.flags[node as usize] & NODE_PRESENT != 0;
    for way in 0..extract.ways.count() {
        let refs = extract.ways.refs(way);
        for (position, &node) in refs.iter().enumerate() {
            let boundary = position == 0 || position + 1 == refs.len();
            let next_to_missing = position > 0 && !present(refs[position - 1]) || position + 1 < refs.len() && !present(refs[position + 1]);
            let weight = if boundary || next_to_missing { 2 } else { 1 };
            uses[node as usize] = uses[node as usize].saturating_add(weight);
        }
    }
    let mut next = 0u32;
    let ids = (0..node_count)
        .map(|node| {
            let flags = extract.nodes.flags[node];
            if uses[node] >= 2 && flags & NODE_PRESENT != 0 && flags & NODE_BLOCKS_CARS == 0 {
                next += 1;
                next - 1
            } else {
                NOT_JUNCTION
            }
        })
        .collect();
    (ids, next as usize)
}

pub fn build(extract: &OsmExtract) -> Topology {
    let (junction_id, junction_count) = junction_ids(extract);
    let ctx = Context { extract, junction_id: &junction_id };
    let chunks: Vec<ChunkChains> = (0..extract.ways.count())
        .into_par_iter()
        .chunks(4096)
        .map(|ways| {
            let mut chunk = ChunkChains::default();
            for way in ways {
                chunk.walk(&ctx, way);
            }
            chunk
        })
        .collect();
    let mut node_coords = vec![Coord { lat: 0, lon: 0 }; junction_count];
    for (node, &id) in junction_id.iter().enumerate() {
        if id != NOT_JUNCTION {
            node_coords[id as usize] = extract.nodes.coords[node];
        }
    }
    let chain_count = chunks.iter().map(|c| c.chains.len()).sum();
    let interior_count = chunks.iter().map(|c| c.interior.len()).sum();
    let restrictions = extract
        .restrictions
        .iter()
        .filter(|r| junction_id[r.via as usize] != NOT_JUNCTION)
        .map(|r| GraphRestriction { rule: r.rule, from: r.from.clone(), via: junction_id[r.via as usize], to: r.to.clone() })
        .collect();
    let mut topology = Topology {
        node_coords,
        chains: Vec::with_capacity(chain_count),
        chain_way: Vec::with_capacity(chain_count),
        chain_snappable: Vec::with_capacity(chain_count),
        interior_first: Vec::with_capacity(chain_count + 1),
        interior: Vec::with_capacity(interior_count),
        restrictions,
    };
    topology.interior_first.push(0);
    for chunk in chunks {
        let fresh_base = topology.node_coords.len() as u32;
        let resolve = |end: End| match end {
            End::Junction(id) => id,
            End::Fresh(local) => fresh_base + local,
        };
        topology.node_coords.extend(&chunk.fresh_coords);
        topology.interior.extend(&chunk.interior);
        for (tail, head, cost, interior_len, way) in chunk.chains {
            topology.chains.push(Chain { tail: resolve(tail), head: resolve(head), cost });
            topology.chain_way.push(way);
            topology.chain_snappable.push(extract.ways.profiles[way as usize].snappable);
            topology.interior_first.push(topology.interior_first.last().expect("starts with zero") + interior_len as u64);
        }
    }
    topology
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simplification_keeps_endpoints_and_significant_bends() {
        let straight: Vec<Coord> = (0..10).map(|i| Coord::from_degrees(54.0, 10.0 + i as f64 * 0.0001)).collect();
        assert_eq!(simplify(&straight, 1.0), vec![straight[0], straight[9]]);
        let bent = vec![Coord::from_degrees(54.0, 10.0), Coord::from_degrees(54.001, 10.001), Coord::from_degrees(54.0, 10.002)];
        assert_eq!(simplify(&bent, 5.0), bent);
        assert_eq!(simplify(&bent, 500.0), vec![bent[0], bent[2]]);
    }
}
