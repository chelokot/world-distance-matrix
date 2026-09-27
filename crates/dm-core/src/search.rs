use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::network::{ChArc, Hierarchy, ARC_BACKWARD, ARC_FORWARD};
use crate::weight::{Weight, UNREACHABLE};

#[repr(C)]
#[derive(Clone, Copy)]
struct Slot {
    key: u32,
    stamp: u32,
    value: Weight,
}

pub struct SparseMap {
    slots: Vec<Slot>,
    stamp: u32,
    len: usize,
}

impl SparseMap {
    pub fn new() -> Self {
        Self::with_capacity(1 << 10)
    }

    fn with_capacity(capacity: usize) -> Self {
        debug_assert!(capacity.is_power_of_two());
        Self { slots: vec![Slot { key: 0, stamp: 0, value: UNREACHABLE }; capacity], stamp: 1, len: 0 }
    }

    fn home(&self, key: u32) -> usize {
        (key.wrapping_mul(0x9E37_79B1) as usize) & (self.slots.len() - 1)
    }

    fn position(&self, key: u32) -> usize {
        let mask = self.slots.len() - 1;
        let mut index = self.home(key);
        loop {
            let slot = &self.slots[index];
            if slot.stamp != self.stamp || slot.key == key {
                return index;
            }
            index = (index + 1) & mask;
        }
    }

    pub fn get(&self, key: u32) -> Weight {
        let slot = &self.slots[self.position(key)];
        if slot.stamp == self.stamp {
            slot.value
        } else {
            UNREACHABLE
        }
    }

    pub fn improve(&mut self, key: u32, value: Weight) -> bool {
        let index = self.position(key);
        let stamp = self.stamp;
        let slot = &mut self.slots[index];
        if slot.stamp == stamp {
            if value < slot.value {
                slot.value = value;
                return true;
            }
            return false;
        }
        *slot = Slot { key, stamp, value };
        self.len += 1;
        if self.len * 2 > self.slots.len() {
            self.grow();
        }
        true
    }

    fn grow(&mut self) {
        let mut bigger = Self::with_capacity(self.slots.len() * 2);
        for slot in self.slots.iter().filter(|slot| slot.stamp == self.stamp) {
            let index = bigger.position(slot.key);
            bigger.slots[index] = Slot { key: slot.key, stamp: bigger.stamp, value: slot.value };
        }
        bigger.len = self.len;
        *self = bigger;
    }

    pub fn clear(&mut self) {
        self.len = 0;
        if self.stamp == u32::MAX {
            self.slots.iter_mut().for_each(|slot| slot.stamp = 0);
            self.stamp = 0;
        }
        self.stamp += 1;
    }
}

impl Default for SparseMap {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

fn leading(arcs: &[ChArc], flag: u32) -> impl Iterator<Item = &ChArc> {
    arcs.iter().take_while(move |arc| arc.has(flag))
}

fn trailing(arcs: &[ChArc], flag: u32) -> impl Iterator<Item = &ChArc> {
    arcs.iter().rev().take_while(move |arc| arc.has(flag))
}

#[derive(Default)]
pub struct UpwardSearch {
    distances: SparseMap,
    queue: BinaryHeap<Reverse<(Weight, u32)>>,
    settled: Vec<(u32, Weight)>,
}

impl UpwardSearch {
    pub fn run(&mut self, hierarchy: &Hierarchy, seeds: &[(u32, Weight)], direction: Direction) -> &[(u32, Weight)] {
        match direction {
            Direction::Forward => self.search::<true>(hierarchy, seeds),
            Direction::Backward => self.search::<false>(hierarchy, seeds),
        }
        &self.settled
    }

    fn stalled<'a>(&self, mut candidates: impl Iterator<Item = &'a ChArc>, distance: Weight) -> bool {
        candidates.any(|arc| self.distances.get(arc.head()) + arc.weight() < distance)
    }

    fn relax<'a>(&mut self, arcs: impl Iterator<Item = &'a ChArc>, distance: Weight) {
        for arc in arcs {
            let candidate = distance + arc.weight();
            if self.distances.improve(arc.head(), candidate) {
                self.queue.push(Reverse((candidate, arc.head())));
            }
        }
    }

    fn search<const FORWARD: bool>(&mut self, hierarchy: &Hierarchy, seeds: &[(u32, Weight)]) {
        self.distances.clear();
        self.queue.clear();
        self.settled.clear();
        for &(node, weight) in seeds {
            if self.distances.improve(node, weight) {
                self.queue.push(Reverse((weight, node)));
            }
        }
        while let Some(Reverse((distance, node))) = self.queue.pop() {
            if distance > self.distances.get(node) {
                continue;
            }
            let arcs = hierarchy.arcs(node);
            let stalled = if FORWARD { self.stalled(trailing(arcs, ARC_BACKWARD), distance) } else { self.stalled(leading(arcs, ARC_FORWARD), distance) };
            if stalled {
                continue;
            }
            self.settled.push((node, distance));
            if FORWARD {
                self.relax(leading(arcs, ARC_FORWARD), distance);
            } else {
                self.relax(trailing(arcs, ARC_BACKWARD), distance);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    #[test]
    fn sparse_map_behaves_like_a_min_map_across_clears_and_growth() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let mut map = SparseMap::with_capacity(8);
        for _ in 0..5 {
            let mut reference = std::collections::HashMap::new();
            for _ in 0..2000 {
                let key = rng.gen_range(0..700u32);
                let value = rng.gen_range(0..1_000_000u64);
                let expected = reference.get(&key).is_none_or(|&old| value < old);
                if expected {
                    reference.insert(key, value);
                }
                assert_eq!(map.improve(key, value), expected);
            }
            for key in 0..800u32 {
                assert_eq!(map.get(key), reference.get(&key).copied().unwrap_or(UNREACHABLE));
            }
            map.clear();
            assert_eq!(map.get(3), UNREACHABLE);
        }
    }
}
