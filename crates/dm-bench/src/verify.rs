use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use dm_core::matrix::{Endpoint, MatrixJob};
use dm_core::network::Network;
use dm_core::weight::{scale, RouteValue, Weight, UNREACHABLE, UNREACHABLE_VALUE};
use rayon::prelude::*;

pub struct Matrix {
    pub size: usize,
    pub cells: Vec<RouteValue>,
}

impl Matrix {
    pub fn at(&self, row: usize, col: usize) -> RouteValue {
        self.cells[row * self.size + col]
    }
}

pub fn engine_matrix(network: &Network, endpoints: &[Endpoint]) -> Matrix {
    let job = MatrixJob::prepare(network, endpoints.to_vec(), endpoints.to_vec());
    let n = endpoints.len();
    let mut out = vec![0u32; n * n * 2];
    job.compute_rows(0..n, &mut out);
    let cells = out
        .chunks_exact(2 * n.max(1))
        .flat_map(|row| {
            let (d, t) = row.split_at(n);
            d.iter().zip(t).map(|(&distance_m, &duration_s)| RouteValue { distance_m, duration_s }).collect::<Vec<_>>()
        })
        .collect();
    Matrix { size: n, cells }
}

struct ReferenceGraph {
    first: Vec<u32>,
    heads: Vec<u32>,
    weights: Vec<Weight>,
    point_node: Vec<Option<u32>>,
}

fn reference_graph(network: &Network, endpoints: &[Endpoint]) -> ReferenceGraph {
    let node_count = network.manifest.node_count as usize;
    let chains = &network.chains;
    let turns = &network.turns;
    let mut on_chain: HashMap<u32, Vec<(f64, usize)>> = HashMap::new();
    for (index, endpoint) in endpoints.iter().enumerate() {
        if let Some(snap) = endpoint.snap {
            on_chain.entry(snap.placement.chain).or_default().push((snap.placement.fraction, index));
        }
    }
    let mut point_node = vec![None; endpoints.len()];
    let mut next_virtual = node_count as u32;
    for points in on_chain.values_mut() {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        for &(_, index) in points.iter() {
            point_node[index] = Some(next_virtual);
            next_virtual += 1;
        }
    }
    let arcs_of = |chain: u32, emit: &mut dyn FnMut(u32, u32, Weight)| {
        let c = chain as usize;
        let cost = chains.cost[c];
        let points: Vec<(f64, u32)> =
            on_chain.get(&chain).map_or_else(Vec::new, |list| list.iter().map(|&(f, i)| (f, point_node[i].expect("assigned"))).collect());
        for (backward, weight, from, to) in [(false, cost.forward(), chains.tail[c], chains.head[c]), (true, cost.backward(), chains.head[c], chains.tail[c])] {
            let Some(weight) = weight else { continue };
            let arrival = turns.arrival(chain, backward).unwrap_or(to);
            let departures = std::iter::once(from).chain(turns.departures(chain, backward));
            if points.is_empty() {
                departures.for_each(|d| emit(d, arrival, weight));
                continue;
            }
            let ordered: Vec<(f64, u32)> = if backward { points.iter().rev().map(|&(f, n)| (1.0 - f, n)).collect() } else { points.clone() };
            let (first_fraction, first_node) = ordered[0];
            departures.for_each(|d| emit(d, first_node, scale(weight, first_fraction)));
            for pair in ordered.windows(2) {
                emit(pair[0].1, pair[1].1, scale(weight, pair[1].0 - pair[0].0));
            }
            let (last_fraction, last_node) = ordered[ordered.len() - 1];
            emit(last_node, arrival, scale(weight, 1.0 - last_fraction));
        }
    };
    let total = next_virtual as usize;
    let mut first = vec![0u32; total + 1];
    for chain in 0..chains.count() as u32 {
        arcs_of(chain, &mut |tail, _, _| first[tail as usize + 1] += 1);
    }
    for i in 0..total {
        first[i + 1] += first[i];
    }
    let mut cursor = first.clone();
    let mut heads = vec![0u32; first[total] as usize];
    let mut weights = vec![0u64; first[total] as usize];
    for chain in 0..chains.count() as u32 {
        arcs_of(chain, &mut |tail, head, weight| {
            let slot = cursor[tail as usize] as usize;
            heads[slot] = head;
            weights[slot] = weight;
            cursor[tail as usize] += 1;
        });
    }
    ReferenceGraph { first, heads, weights, point_node }
}

pub fn path_polyline(network: &Network, from: Endpoint, to: Endpoint) -> Vec<(f64, f64)> {
    let endpoints = [from, to];
    let graph = reference_graph(network, &endpoints);
    let (source, target) = (graph.point_node[0].expect("snapped source"), graph.point_node[1].expect("snapped target"));
    let mut distance: HashMap<u32, Weight> = HashMap::from([(source, 0)]);
    let mut previous: HashMap<u32, u32> = HashMap::new();
    let mut queue = BinaryHeap::from([Reverse((0u64, source))]);
    while let Some(Reverse((d, v))) = queue.pop() {
        if d > distance[&v] || v == target {
            continue;
        }
        for arc in graph.first[v as usize] as usize..graph.first[v as usize + 1] as usize {
            let (head, candidate) = (graph.heads[arc], d + graph.weights[arc]);
            if distance.get(&head).is_none_or(|&old| candidate < old) {
                distance.insert(head, candidate);
                previous.insert(head, v);
                queue.push(Reverse((candidate, head)));
            }
        }
    }
    let node_count = network.manifest.node_count;
    let mut nodes = vec![target];
    while let Some(&p) = previous.get(nodes.last().expect("non-empty")) {
        nodes.push(p);
    }
    nodes.reverse();
    nodes
        .into_iter()
        .filter(|&n| n < node_count)
        .map(|n| {
            let c = network.node_coords[n as usize];
            (c.lat_degrees(), c.lon_degrees())
        })
        .collect()
}

fn dijkstra_row(graph: &ReferenceGraph, source: u32, targets: &[Option<u32>]) -> Vec<Weight> {
    let mut distance: HashMap<u32, Weight> = HashMap::new();
    let mut queue = BinaryHeap::new();
    let mut remaining: HashSet<u32> = targets.iter().flatten().copied().collect();
    distance.insert(source, 0);
    queue.push(Reverse((0u64, source)));
    while let Some(Reverse((d, v))) = queue.pop() {
        if d > distance[&v] {
            continue;
        }
        remaining.remove(&v);
        if remaining.is_empty() {
            break;
        }
        for arc in graph.first[v as usize] as usize..graph.first[v as usize + 1] as usize {
            let (head, candidate) = (graph.heads[arc], d + graph.weights[arc]);
            if distance.get(&head).is_none_or(|&old| candidate < old) {
                distance.insert(head, candidate);
                queue.push(Reverse((candidate, head)));
            }
        }
    }
    targets.iter().map(|t| t.and_then(|t| distance.get(&t).copied()).unwrap_or(UNREACHABLE)).collect()
}

#[derive(Default, Debug)]
pub struct Report {
    pub pairs: usize,
    pub exact: usize,
    pub within_tolerance: usize,
    pub reachability_mismatch: usize,
    pub duration_mismatch: usize,
    pub distance_only_mismatch: usize,
    pub max_duration_error_s: u32,
    pub max_distance_error_m: u32,
    pub diagonal_violations: usize,
    pub triangle_violations: usize,
    pub nondeterministic_cells: usize,
    pub permutation_mismatches: usize,
}

pub fn verify(network: &Network, endpoints: &[Endpoint], reference_sources: usize) -> Report {
    let matrix = engine_matrix(network, endpoints);
    let graph = reference_graph(network, endpoints);
    let n = endpoints.len();
    let access: Vec<Weight> = endpoints.iter().map(|e| e.snap.map_or(0, |s| s.access_leg())).collect();
    let targets: Vec<Option<u32>> = graph.point_node.clone();
    let rows: Vec<(usize, Vec<Weight>)> = (0..n.min(reference_sources))
        .into_par_iter()
        .map(|source| {
            let row = match graph.point_node[source] {
                Some(node) => dijkstra_row(&graph, node, &targets),
                None => vec![UNREACHABLE; n],
            };
            (source, row)
        })
        .collect();
    let mut report = Report::default();
    for (source, row) in rows {
        for (target, &weight) in row.iter().enumerate() {
            report.pairs += 1;
            let expected = if endpoints[source].coord == endpoints[target].coord {
                RouteValue { distance_m: 0, duration_s: 0 }
            } else {
                RouteValue::from_weight(weight + access[source] + access[target])
            };
            let actual = matrix.at(source, target);
            if expected == actual {
                report.exact += 1;
                report.within_tolerance += 1;
                continue;
            }
            if (expected.duration_s == UNREACHABLE_VALUE) != (actual.duration_s == UNREACHABLE_VALUE) {
                report.reachability_mismatch += 1;
                continue;
            }
            let duration_error = expected.duration_s.abs_diff(actual.duration_s);
            let distance_error = expected.distance_m.abs_diff(actual.distance_m);
            report.max_duration_error_s = report.max_duration_error_s.max(duration_error);
            report.max_distance_error_m = report.max_distance_error_m.max(distance_error);
            if duration_error <= 1 && distance_error <= 2 {
                report.within_tolerance += 1;
            } else if duration_error <= 1 {
                report.distance_only_mismatch += 1;
            } else {
                report.duration_mismatch += 1;
            }
        }
    }
    for i in 0..n {
        if matrix.at(i, i) != (RouteValue { distance_m: 0, duration_s: 0 }) {
            report.diagonal_violations += 1;
        }
    }
    let sample = n.min(150);
    for i in 0..sample {
        for j in 0..sample {
            for k in 0..sample {
                let (ij, jk, ik) = (matrix.at(i, j).duration_s, matrix.at(j, k).duration_s, matrix.at(i, k).duration_s);
                if ij != UNREACHABLE_VALUE && jk != UNREACHABLE_VALUE && (ik == UNREACHABLE_VALUE || ik as u64 > ij as u64 + jk as u64 + 1) {
                    report.triangle_violations += 1;
                }
            }
        }
    }
    let again = engine_matrix(network, endpoints);
    report.nondeterministic_cells = matrix.cells.iter().zip(&again.cells).filter(|(a, b)| a != b).count();
    let permuted: Vec<Endpoint> = endpoints.iter().rev().copied().collect();
    let reversed = engine_matrix(network, &permuted);
    for i in 0..n {
        for j in 0..n {
            if reversed.at(n - 1 - i, n - 1 - j) != matrix.at(i, j) {
                report.permutation_mismatches += 1;
            }
        }
    }
    report
}
