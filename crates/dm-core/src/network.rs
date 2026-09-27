use std::path::Path;

use anyhow::{ensure, Result};
use bytemuck::{Pod, Zeroable};
use rayon::prelude::*;

use crate::geo::Coord;
use crate::spatial::{BBox, PackedRtree};
use crate::store::{Array, ArrayReader, Manifest, Residency};
use crate::weight::{pack, ChainCost, Weight};

pub const ARC_FORWARD: u32 = 1 << 31;
pub const ARC_BACKWARD: u32 = 1 << 30;
pub const NODE_MASK: u32 = ARC_BACKWARD - 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct ChArc {
    pub head_and_flags: u32,
    pub time_ms: u32,
    pub dist_dm: u32,
}

impl ChArc {
    pub fn new(head: u32, flags: u32, weight: Weight) -> Self {
        debug_assert!(head <= NODE_MASK && flags & NODE_MASK == 0);
        Self {
            head_and_flags: head | flags,
            time_ms: u32::try_from(crate::weight::time_ms(weight)).expect("a hierarchy arc takes less than 49 days"),
            dist_dm: crate::weight::dist_dm(weight),
        }
    }

    pub fn head(&self) -> u32 {
        self.head_and_flags & NODE_MASK
    }

    pub fn has(&self, flag: u32) -> bool {
        self.head_and_flags & flag != 0
    }

    pub fn weight(&self) -> Weight {
        pack(self.time_ms, self.dist_dm)
    }

    pub fn group(&self) -> u8 {
        match (self.has(ARC_FORWARD), self.has(ARC_BACKWARD)) {
            (true, false) => 0,
            (true, true) => 1,
            _ => 2,
        }
    }
}

pub struct Hierarchy {
    pub first_arc: Array<u32>,
    pub arcs: Array<ChArc>,
}

impl Hierarchy {
    pub fn node_count(&self) -> usize {
        self.first_arc.len() - 1
    }

    pub fn arcs(&self, node: u32) -> &[ChArc] {
        &self.arcs[self.first_arc[node as usize] as usize..self.first_arc[node as usize + 1] as usize]
    }
}

pub fn turn_key(chain: u32, backward: bool) -> u32 {
    chain * 2 + backward as u32
}

pub struct Turns {
    pub arrivals: Array<[u32; 2]>,
    pub departures: Array<[u32; 2]>,
}

impl Turns {
    pub fn arrival(&self, chain: u32, backward: bool) -> Option<u32> {
        let key = turn_key(chain, backward);
        self.arrivals.binary_search_by_key(&key, |entry| entry[0]).ok().map(|slot| self.arrivals[slot][1])
    }

    pub fn departures(&self, chain: u32, backward: bool) -> impl Iterator<Item = u32> + Clone + '_ {
        let key = turn_key(chain, backward);
        let start = self.departures.partition_point(|entry| entry[0] < key);
        self.departures[start..].iter().take_while(move |entry| entry[0] == key).map(|entry| entry[1])
    }
}

pub struct Chains {
    pub tail: Array<u32>,
    pub head: Array<u32>,
    pub cost: Array<ChainCost>,
    pub geometry_first: Array<u32>,
    pub geometry: Array<Coord>,
}

impl Chains {
    pub fn count(&self) -> usize {
        self.tail.len()
    }

    pub fn polyline<'a>(&'a self, chain: u32, node_coords: &'a [Coord]) -> impl Iterator<Item = Coord> + 'a {
        let chain = chain as usize;
        let interior = &self.geometry[self.geometry_first[chain] as usize..self.geometry_first[chain + 1] as usize];
        std::iter::once(node_coords[self.tail[chain] as usize]).chain(interior.iter().copied()).chain(std::iter::once(node_coords[self.head[chain] as usize]))
    }
}

pub struct Network {
    pub manifest: Manifest,
    pub hierarchy: Hierarchy,
    pub chains: Chains,
    pub turns: Turns,
    pub node_coords: Array<Coord>,
    pub major_index: PackedRtree,
    pub minor_index: PackedRtree,
}

impl Network {
    pub fn open(dir: &Path, residency: Residency) -> Result<Self> {
        let reader = ArrayReader::open(dir, residency)?;
        let manifest = reader.manifest().clone();
        let network = Self {
            hierarchy: Hierarchy { first_arc: reader.read("ch_first_arc")?, arcs: reader.read("ch_arcs")? },
            chains: Chains {
                tail: reader.read("chain_tail")?,
                head: reader.read("chain_head")?,
                cost: reader.read("chain_cost")?,
                geometry_first: reader.read("chain_geometry_first")?,
                geometry: reader.read("chain_geometry")?,
            },
            turns: Turns { arrivals: reader.read("turn_arrivals")?, departures: reader.read("turn_departures")? },
            node_coords: reader.read("node_coords")?,
            major_index: PackedRtree::new(0, manifest.major_chain_count, reader.read::<BBox>("major_index")?),
            minor_index: PackedRtree::new(manifest.major_chain_count, manifest.chain_count - manifest.major_chain_count, reader.read::<BBox>("minor_index")?),
            manifest,
        };
        network.validate()?;
        Ok(network)
    }

    fn validate(&self) -> Result<()> {
        let nodes = self.manifest.node_count as usize;
        let chains = self.manifest.chain_count as usize;
        ensure!(self.hierarchy.first_arc.len() == nodes + 1, "hierarchy node count mismatch");
        ensure!(self.hierarchy.first_arc[nodes] as usize == self.hierarchy.arcs.len(), "hierarchy arc count mismatch");
        ensure!(self.node_coords.len() == nodes, "node coordinate count mismatch");
        ensure!(self.chains.tail.len() == chains && self.chains.head.len() == chains && self.chains.cost.len() == chains, "chain count mismatch");
        ensure!(self.chains.geometry_first.len() == chains + 1, "chain geometry index mismatch");
        ensure!(self.chains.geometry_first[chains] as usize == self.chains.geometry.len(), "chain geometry length mismatch");
        let grouped = (0..nodes as u32).into_par_iter().all(|node| self.hierarchy.arcs(node).windows(2).all(|pair| pair[0].group() <= pair[1].group()));
        ensure!(grouped, "hierarchy arcs are not grouped by direction");
        ensure!(self.turns.arrivals.windows(2).all(|pair| pair[0][0] < pair[1][0]), "turn arrivals are not sorted by key");
        ensure!(self.turns.departures.windows(2).all(|pair| pair[0] <= pair[1]), "turn departures are not sorted by key");
        Ok(())
    }
}
