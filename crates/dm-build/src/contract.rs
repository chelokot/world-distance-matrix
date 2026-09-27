use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

use dm_core::geo::{hilbert_index, Coord};
use dm_core::network::{ChArc, ARC_BACKWARD, ARC_FORWARD, NODE_MASK};
use dm_core::search::SparseMap;
use dm_core::weight::{Weight, UNREACHABLE};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug)]
struct Edge {
    other: u32,
    hops: u32,
    forward: Weight,
    backward: Weight,
}

#[derive(Clone, Copy, Debug)]
struct Shortcut {
    from: u32,
    to: u32,
    weight: Weight,
    hops: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Active,
    Selected,
    Contracted,
}

pub struct Hierarchy {
    pub rank: Vec<u32>,
    pub first_arc: Vec<u32>,
    pub arcs: Vec<ChArc>,
}

struct Witness {
    distances: SparseMap,
    queue: BinaryHeap<Reverse<(Weight, u32)>>,
}

impl Witness {
    fn new() -> Self {
        Self { distances: SparseMap::new(), queue: BinaryHeap::new() }
    }

    fn search(&mut self, graph: &[Vec<Edge>], state: &[State], source: u32, avoid: u32, limit: Weight, settle_limit: usize) {
        self.distances.clear();
        self.queue.clear();
        self.distances.improve(source, 0);
        self.queue.push(Reverse((0, source)));
        let mut settled = 0;
        while let Some(Reverse((distance, node))) = self.queue.pop() {
            if distance > self.distances.get(node) {
                continue;
            }
            if distance > limit || settled == settle_limit {
                break;
            }
            settled += 1;
            for edge in &graph[node as usize] {
                if edge.forward < UNREACHABLE && edge.other != avoid && state[edge.other as usize] == State::Active {
                    let candidate = distance + edge.forward;
                    if self.distances.improve(edge.other, candidate) {
                        self.queue.push(Reverse((candidate, edge.other)));
                    }
                }
            }
        }
    }

    fn shortcuts(&mut self, graph: &[Vec<Edge>], state: &[State], node: u32, settle_limit: usize, out: &mut Vec<Shortcut>) {
        out.clear();
        let edges = &graph[node as usize];
        for incoming in edges.iter().filter(|e| e.backward < UNREACHABLE) {
            let from = incoming.other;
            let outgoing = || edges.iter().filter(move |e| e.forward < UNREACHABLE && e.other != from);
            let Some(longest) = outgoing().map(|e| e.forward).max() else { continue };
            self.search(graph, state, from, node, incoming.backward + longest, settle_limit);
            for target in outgoing() {
                let via = incoming.backward + target.forward;
                if self.distances.get(target.other) > via {
                    out.push(Shortcut { from, to: target.other, weight: via, hops: incoming.hops + target.hops });
                }
            }
        }
    }
}

fn priority(level: u32, edges: &[Edge], shortcuts: &[Shortcut]) -> f32 {
    let (removed_arcs, removed_hops) = edges.iter().fold((0u32, 0u32), |(arcs, hops), e| {
        let directions = (e.forward < UNREACHABLE) as u32 + (e.backward < UNREACHABLE) as u32;
        (arcs + directions, hops + directions * e.hops)
    });
    let added_hops: u32 = shortcuts.iter().map(|s| s.hops).sum();
    level as f32 + shortcuts.len() as f32 / removed_arcs.max(1) as f32 + added_hops as f32 / removed_hops.max(1) as f32
}

fn upsert(edges: &mut Vec<Edge>, other: u32, forward: Weight, backward: Weight, hops: u32) {
    match edges.iter_mut().find(|e| e.other == other) {
        Some(edge) => {
            if forward < edge.forward || backward < edge.backward {
                edge.hops = edge.hops.max(hops);
            }
            edge.forward = edge.forward.min(forward);
            edge.backward = edge.backward.min(backward);
        }
        None => edges.push(Edge { other, hops, forward, backward }),
    }
}

fn tie_break(node: u32) -> u64 {
    let mut x = node as u64 ^ 0x9E37_79B9_7F4A_7C15;
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

pub struct Params {
    pub witness_settle_limit: usize,
}

pub fn contract(node_coords: &[Coord], arcs: impl Iterator<Item = (u32, u32, Weight)>, params: &Params) -> Hierarchy {
    let node_count = node_coords.len();
    assert!(node_count <= NODE_MASK as usize, "too many nodes for the arc encoding");
    let mut graph: Vec<Vec<Edge>> = vec![Vec::new(); node_count];
    for (tail, head, weight) in arcs {
        if tail != head {
            upsert(&mut graph[tail as usize], head, weight, UNREACHABLE, 1);
            upsert(&mut graph[head as usize], tail, UNREACHABLE, weight, 1);
        }
    }
    let settle_limit = params.witness_settle_limit;
    let mut state = vec![State::Active; node_count];
    let mut level = vec![0u32; node_count];
    let mut priorities: Vec<f32> = (0..node_count as u32)
        .into_par_iter()
        .map_init(
            || (Witness::new(), Vec::new()),
            |(witness, shortcuts), node| {
                witness.shortcuts(&graph, &state, node, settle_limit, shortcuts);
                priority(0, &graph[node as usize], shortcuts)
            },
        )
        .collect();
    let mut active: Vec<u32> = (0..node_count as u32).collect();
    let mut order: Vec<u32> = Vec::with_capacity(node_count);
    let mut up_first: Vec<u64> = Vec::with_capacity(node_count + 1);
    up_first.push(0);
    let mut up_arcs: Vec<ChArc> = Vec::new();
    let started = Instant::now();
    let mut rounds = 0usize;
    let mut last_report = Instant::now();
    while !active.is_empty() {
        rounds += 1;
        let key = |node: u32| (priorities[node as usize], tie_break(node), node);
        let mut selected: Vec<u32> = active
            .par_iter()
            .copied()
            .filter(|&node| {
                let own = key(node);
                graph[node as usize].iter().all(|e| key(e.other).partial_cmp(&own) == Some(std::cmp::Ordering::Greater))
            })
            .collect();
        selected.par_sort_by_cached_key(|&node| hilbert_index(node_coords[node as usize]));
        for &node in &selected {
            state[node as usize] = State::Selected;
        }
        let shortcuts: Vec<Vec<Shortcut>> = selected
            .par_iter()
            .map_init(Witness::new, |witness, &node| {
                let mut out = Vec::new();
                witness.shortcuts(&graph, &state, node, settle_limit, &mut out);
                out
            })
            .collect();
        let mut touched: Vec<u32> = Vec::new();
        for (&node, node_shortcuts) in selected.iter().zip(shortcuts) {
            order.push(node);
            let edges = std::mem::take(&mut graph[node as usize]);
            for edge in &edges {
                let neighbour = &mut graph[edge.other as usize];
                let position = neighbour.iter().position(|e| e.other == node).expect("adjacency is symmetric");
                neighbour.swap_remove(position);
                level[edge.other as usize] = level[edge.other as usize].max(level[node as usize] + 1);
                touched.push(edge.other);
                if edge.forward == edge.backward {
                    up_arcs.push(ChArc::new(edge.other, ARC_FORWARD | ARC_BACKWARD, edge.forward));
                    continue;
                }
                if edge.forward < UNREACHABLE {
                    up_arcs.push(ChArc::new(edge.other, ARC_FORWARD, edge.forward));
                }
                if edge.backward < UNREACHABLE {
                    up_arcs.push(ChArc::new(edge.other, ARC_BACKWARD, edge.backward));
                }
            }
            let node_arcs_start = *up_first.last().expect("starts with zero") as usize;
            up_arcs[node_arcs_start..].sort_unstable_by_key(ChArc::group);
            up_first.push(up_arcs.len() as u64);
            state[node as usize] = State::Contracted;
            for s in node_shortcuts {
                upsert(&mut graph[s.from as usize], s.to, s.weight, UNREACHABLE, s.hops);
                upsert(&mut graph[s.to as usize], s.from, UNREACHABLE, s.weight, s.hops);
            }
        }
        touched.par_sort_unstable();
        touched.dedup();
        let updated: Vec<f32> = touched
            .par_iter()
            .map_init(
                || (Witness::new(), Vec::new()),
                |(witness, shortcuts), &node| {
                    witness.shortcuts(&graph, &state, node, settle_limit, shortcuts);
                    priority(level[node as usize], &graph[node as usize], shortcuts)
                },
            )
            .collect();
        for (&node, value) in touched.iter().zip(updated) {
            priorities[node as usize] = value;
        }
        active = active.into_par_iter().filter(|&node| state[node as usize] == State::Active).collect();
        if last_report.elapsed() >= Duration::from_secs(30) {
            last_report = Instant::now();
            tracing::info!(
                rounds,
                contracted = selected.len(),
                remaining = active.len(),
                arcs = up_arcs.len(),
                elapsed_s = started.elapsed().as_secs_f32(),
                "contracting"
            );
        }
    }
    tracing::info!(rounds, arcs = up_arcs.len(), elapsed_s = started.elapsed().as_secs_f32(), "contraction finished");
    let mut rank = vec![0u32; node_count];
    for (position, &node) in order.iter().enumerate() {
        rank[node as usize] = position as u32;
    }
    assert!(up_arcs.len() < u32::MAX as usize, "too many arcs for 32-bit offsets");
    up_arcs.par_iter_mut().for_each(|arc| {
        let flags = arc.head_and_flags & !NODE_MASK;
        arc.head_and_flags = rank[arc.head() as usize] | flags;
    });
    let first_arc = up_first.into_iter().map(|offset| offset as u32).collect();
    Hierarchy { rank, first_arc, arcs: up_arcs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dm_core::network::Hierarchy as QueryHierarchy;
    use dm_core::search::{Direction, UpwardSearch};
    use dm_core::weight::pack;
    use rand::{Rng, SeedableRng};

    fn dijkstra(node_count: usize, arcs: &[(u32, u32, Weight)], source: u32) -> Vec<Weight> {
        let mut distances = vec![UNREACHABLE; node_count];
        let mut queue = BinaryHeap::new();
        distances[source as usize] = 0;
        queue.push(Reverse((0, source)));
        while let Some(Reverse((d, v))) = queue.pop() {
            if d > distances[v as usize] {
                continue;
            }
            for &(_, head, w) in arcs.iter().filter(|a| a.0 == v) {
                let candidate = d + w;
                if candidate < distances[head as usize] {
                    distances[head as usize] = candidate;
                    queue.push(Reverse((candidate, head)));
                }
            }
        }
        distances
    }

    fn random_graph(seed: u64, node_count: usize) -> (Vec<Coord>, Vec<(u32, u32, Weight)>) {
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let coords: Vec<Coord> = (0..node_count).map(|_| Coord::from_degrees(rng.gen_range(54.0..54.1), rng.gen_range(10.0..10.1))).collect();
        let mut arcs = Vec::new();
        for tail in 0..node_count as u32 {
            for _ in 0..rng.gen_range(1..4) {
                let head = rng.gen_range(0..node_count as u32);
                let weight = pack(rng.gen_range(1..50), rng.gen_range(1..500));
                arcs.push((tail, head, weight));
                if rng.gen_bool(0.7) {
                    let back = if rng.gen_bool(0.8) { weight } else { pack(rng.gen_range(1..50), rng.gen_range(1..500)) };
                    arcs.push((head, tail, back));
                }
            }
        }
        (coords, arcs)
    }

    #[test]
    fn hierarchy_preserves_all_pairs_shortest_paths() {
        for seed in 0..12 {
            let node_count = 60 + seed as usize * 7;
            let (coords, arcs) = random_graph(seed, node_count);
            let built = contract(&coords, arcs.iter().copied(), &Params { witness_settle_limit: if seed % 2 == 0 { 3 } else { 500 } });
            let hierarchy = QueryHierarchy { first_arc: built.first_arc.clone().into(), arcs: built.arcs.clone().into() };
            let mut forward = UpwardSearch::default();
            let mut backward = UpwardSearch::default();
            for source in 0..node_count as u32 {
                let expected = dijkstra(node_count, &arcs, source);
                let up: std::collections::HashMap<u32, Weight> =
                    forward.run(&hierarchy, &[(built.rank[source as usize], 0)], Direction::Forward).iter().copied().collect();
                for target in 0..node_count as u32 {
                    let down = backward.run(&hierarchy, &[(built.rank[target as usize], 0)], Direction::Backward);
                    let best = down.iter().filter_map(|(node, d)| up.get(node).map(|u| u + d)).min().unwrap_or(UNREACHABLE);
                    assert_eq!(best, expected[target as usize], "seed {seed} {source}->{target}");
                }
            }
        }
    }
}
