use std::ops::Range;

use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::geo::Coord;
use crate::network::Network;
use crate::search::{Direction, UpwardSearch};
use crate::snap::Placement;
use crate::weight::{scale, RouteValue, Weight, UNREACHABLE};

#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    pub coord: Coord,
    pub placement: Option<Placement>,
}

type Seeds = Vec<(u32, Weight)>;

fn source_seeds(network: &Network, placement: Placement) -> Seeds {
    let chain = placement.chain;
    let (tail, head) = (network.chains.tail[chain as usize], network.chains.head[chain as usize]);
    let cost = network.chains.cost[chain as usize];
    let turns = &network.turns;
    let forward = cost.forward().map(|w| (turns.arrival(chain, false).unwrap_or(head), scale(w, 1.0 - placement.fraction)));
    let backward = cost.backward().map(|w| (turns.arrival(chain, true).unwrap_or(tail), scale(w, placement.fraction)));
    forward.into_iter().chain(backward).collect()
}

fn target_seeds(network: &Network, placement: Placement) -> Seeds {
    let chain = placement.chain;
    let (tail, head) = (network.chains.tail[chain as usize], network.chains.head[chain as usize]);
    let cost = network.chains.cost[chain as usize];
    let turns = &network.turns;
    let forward = cost.forward().into_iter().flat_map(|w| {
        let partial = scale(w, placement.fraction);
        std::iter::once(tail).chain(turns.departures(chain, false)).map(move |node| (node, partial))
    });
    let backward = cost.backward().into_iter().flat_map(|w| {
        let partial = scale(w, 1.0 - placement.fraction);
        std::iter::once(head).chain(turns.departures(chain, true)).map(move |node| (node, partial))
    });
    forward.chain(backward).collect()
}

fn along_chain(network: &Network, from: f64, to: f64, chain: u32) -> Weight {
    let cost = network.chains.cost[chain as usize];
    let forward = cost.forward().filter(|_| to >= from).map(|w| scale(w, to - from));
    let backward = cost.backward().filter(|_| to <= from).map(|w| scale(w, from - to));
    forward.into_iter().chain(backward).min().unwrap_or(UNREACHABLE)
}

const SPARSE: u32 = u32::MAX;

struct Buckets {
    width: usize,
    nodes: Vec<u32>,
    dense_row: Vec<u32>,
    first: Vec<u32>,
    targets: Vec<u32>,
    weights: Vec<Weight>,
    dense: Vec<Weight>,
}

fn relax_dense_scalar(row: &mut [Weight], dense: &[Weight], distance: Weight) {
    for (cell, &weight) in row.iter_mut().zip(dense) {
        *cell = (*cell).min(distance + weight);
    }
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
fn relax_dense(row: &mut [Weight], dense: &[Weight], distance: Weight) {
    use std::arch::x86_64::{__m256i, _mm256_add_epi64, _mm256_blendv_epi8, _mm256_cmpgt_epi64, _mm256_loadu_si256, _mm256_set1_epi64x, _mm256_storeu_si256};
    assert_eq!(row.len(), dense.len());
    let vector_len = row.len() / 4 * 4;
    unsafe {
        let offset = _mm256_set1_epi64x(distance as i64);
        for i in (0..vector_len).step_by(4) {
            let cell = _mm256_loadu_si256(row.as_ptr().add(i).cast::<__m256i>());
            let candidate = _mm256_add_epi64(offset, _mm256_loadu_si256(dense.as_ptr().add(i).cast::<__m256i>()));
            let improved = _mm256_cmpgt_epi64(cell, candidate);
            _mm256_storeu_si256(row.as_mut_ptr().add(i).cast::<__m256i>(), _mm256_blendv_epi8(cell, candidate, improved));
        }
    }
    relax_dense_scalar(&mut row[vector_len..], &dense[vector_len..], distance);
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
fn relax_dense(row: &mut [Weight], dense: &[Weight], distance: Weight) {
    relax_dense_scalar(row, dense, distance);
}

impl Buckets {
    fn is_dense(entries: usize, width: usize) -> bool {
        entries >= 8 && entries * 16 >= width
    }

    fn build(network: &Network, targets: &[Endpoint]) -> Self {
        let spaces: Vec<Vec<(u32, Weight)>> = targets
            .par_iter()
            .map_init(UpwardSearch::default, |search, target| match target.placement {
                Some(placement) => search.run(&network.hierarchy, &target_seeds(network, placement), Direction::Backward).to_vec(),
                None => Vec::new(),
            })
            .collect();
        let mut entries: Vec<(u32, u32, Weight)> =
            spaces.iter().enumerate().flat_map(|(target, space)| space.iter().map(move |&(node, weight)| (node, target as u32, weight))).collect();
        entries.par_sort_unstable_by_key(|&(node, target, _)| (node, target));
        let width = targets.len();
        let mut buckets =
            Buckets { width, nodes: Vec::new(), dense_row: Vec::new(), first: Vec::new(), targets: Vec::new(), weights: Vec::new(), dense: Vec::new() };
        for group in entries.chunk_by(|a, b| a.0 == b.0) {
            buckets.nodes.push(group[0].0);
            buckets.first.push(buckets.targets.len() as u32);
            if Self::is_dense(group.len(), width) {
                buckets.dense_row.push((buckets.dense.len() / width) as u32);
                let row_start = buckets.dense.len();
                buckets.dense.resize(row_start + width, UNREACHABLE);
                for &(_, target, weight) in group {
                    buckets.dense[row_start + target as usize] = weight;
                }
            } else {
                buckets.dense_row.push(SPARSE);
                buckets.targets.extend(group.iter().map(|e| e.1));
                buckets.weights.extend(group.iter().map(|e| e.2));
            }
        }
        buckets.first.push(buckets.targets.len() as u32);
        buckets
    }

    fn scan(&self, node: u32, distance: Weight, row: &mut [Weight]) {
        let Ok(bucket) = self.nodes.binary_search(&node) else { return };
        let dense_row = self.dense_row[bucket];
        if dense_row != SPARSE {
            let start = dense_row as usize * self.width;
            relax_dense(row, &self.dense[start..start + self.width], distance);
            return;
        }
        let range = self.first[bucket] as usize..self.first[bucket + 1] as usize;
        for (&target, &weight) in self.targets[range.clone()].iter().zip(&self.weights[range]) {
            let cell = &mut row[target as usize];
            *cell = (*cell).min(distance + weight);
        }
    }
}

pub struct MatrixJob<'n> {
    network: &'n Network,
    sources: Vec<Endpoint>,
    targets: Vec<Endpoint>,
    buckets: Buckets,
    targets_by_chain: FxHashMap<u32, Vec<(u32, f64)>>,
}

impl<'n> MatrixJob<'n> {
    pub fn prepare(network: &'n Network, sources: Vec<Endpoint>, targets: Vec<Endpoint>) -> Self {
        let buckets = Buckets::build(network, &targets);
        let mut targets_by_chain: FxHashMap<u32, Vec<(u32, f64)>> = FxHashMap::default();
        for (index, target) in targets.iter().enumerate() {
            if let Some(placement) = target.placement {
                targets_by_chain.entry(placement.chain).or_default().push((index as u32, placement.fraction));
            }
        }
        Self { network, sources, targets, buckets, targets_by_chain }
    }

    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    pub fn target_count(&self) -> usize {
        self.targets.len()
    }

    fn row(&self, search: &mut UpwardSearch, source: &Endpoint, row: &mut [Weight]) {
        row.fill(UNREACHABLE);
        match source.placement {
            Some(placement) => {
                let space = search.run(&self.network.hierarchy, &source_seeds(self.network, placement), Direction::Forward);
                for &(node, distance) in space {
                    self.buckets.scan(node, distance, row);
                }
                for &(target, fraction) in self.targets_by_chain.get(&placement.chain).into_iter().flatten() {
                    let direct = along_chain(self.network, placement.fraction, fraction, placement.chain);
                    let cell = &mut row[target as usize];
                    *cell = (*cell).min(direct);
                }
            }
            None => {
                for (cell, target) in row.iter_mut().zip(&self.targets) {
                    if target.coord == source.coord {
                        *cell = 0;
                    }
                }
            }
        }
    }

    pub fn compute_rows(&self, rows: Range<usize>, out: &mut [u32]) {
        let width = self.targets.len();
        assert_eq!(out.len(), rows.len() * 2 * width);
        if width == 0 {
            return;
        }
        out.par_chunks_mut(2 * width).zip(&self.sources[rows]).for_each_init(
            || (UpwardSearch::default(), vec![UNREACHABLE; width]),
            |(search, row), (chunk, source)| {
                self.row(search, source, row);
                let (distances, durations) = chunk.split_at_mut(width);
                for ((weight, distance), duration) in row.iter().zip(distances).zip(durations) {
                    let value = RouteValue::from_weight(*weight);
                    *distance = value.distance_m;
                    *duration = value.duration_s;
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    #[test]
    fn vector_and_scalar_relaxation_agree() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        for width in [0, 1, 3, 4, 5, 17, 1000] {
            let pick = |rng: &mut rand::rngs::StdRng| if rng.gen_bool(0.2) { UNREACHABLE } else { rng.gen_range(0..1u64 << 40) };
            let row: Vec<Weight> = (0..width).map(|_| pick(&mut rng)).collect();
            let dense: Vec<Weight> = (0..width).map(|_| pick(&mut rng)).collect();
            let distance = rng.gen_range(0..1u64 << 40);
            let (mut vector, mut scalar) = (row.clone(), row);
            relax_dense(&mut vector, &dense, distance);
            relax_dense_scalar(&mut scalar, &dense, distance);
            assert_eq!(vector, scalar);
        }
    }
}
