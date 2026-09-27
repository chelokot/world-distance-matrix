use std::path::Path;

use anyhow::{ensure, Result};
use dm_core::geo::{hilbert_index, Coord};
use dm_core::network::{turn_key, Turns};
use dm_core::spatial::{build_levels, BBox};
use dm_core::store::{ArrayWriter, Manifest, FORMAT_VERSION};
use dm_core::weight::ChainCost;
use rayon::prelude::*;

use crate::components::Components;
use crate::contract::Hierarchy;
use crate::topology::Topology;

pub struct AssemblyParams {
    pub major_component_min_nodes: u32,
}

pub struct Built<'a> {
    pub topology: &'a Topology,
    pub turns: &'a Turns,
    pub components: &'a Components,
    pub hierarchy: &'a Hierarchy,
}

pub fn write(output: &Path, source: String, profile: &str, built: &Built, params: &AssemblyParams) -> Result<()> {
    let Built { topology, turns, components, hierarchy } = *built;
    let rank = &hierarchy.rank;
    let boxes: Vec<BBox> = (0..topology.chains.len())
        .into_par_iter()
        .map(|chain| {
            let c = &topology.chains[chain];
            let ends = [topology.node_coords[c.tail as usize], topology.node_coords[c.head as usize]];
            topology.interior(chain).iter().chain(&ends).fold(BBox::EMPTY, |mut b, &coord| {
                b.include(coord);
                b
            })
        })
        .collect();
    let is_major = |chain: usize| {
        let c = &topology.chains[chain];
        components.is_major(c.tail, params.major_component_min_nodes) && components.is_major(c.head, params.major_component_min_nodes)
    };
    let mut chain_order: Vec<u32> = (0..topology.chains.len() as u32).collect();
    chain_order.par_sort_by_cached_key(|&chain| (!is_major(chain as usize), hilbert_index(boxes[chain as usize].center())));
    let major_chain_count = chain_order.iter().take_while(|&&chain| is_major(chain as usize)).count();

    let chain_tail: Vec<u32> = chain_order.iter().map(|&c| rank[topology.chains[c as usize].tail as usize]).collect();
    let chain_head: Vec<u32> = chain_order.iter().map(|&c| rank[topology.chains[c as usize].head as usize]).collect();
    let chain_cost: Vec<ChainCost> = chain_order.iter().map(|&c| topology.chains[c as usize].cost).collect();
    let mut geometry_first = Vec::with_capacity(chain_order.len() + 1);
    geometry_first.push(0u64);
    let mut geometry = Vec::new();
    for &chain in &chain_order {
        geometry.extend_from_slice(topology.interior(chain as usize));
        geometry_first.push(geometry.len() as u64);
    }
    ensure!(geometry.len() < u32::MAX as usize, "geometry exceeds 32-bit offsets");
    let geometry_first: Vec<u32> = geometry_first.into_iter().map(|offset| offset as u32).collect();
    let ordered_boxes: Vec<BBox> = chain_order.iter().map(|&c| boxes[c as usize]).collect();
    let major_index = build_levels(&ordered_boxes[..major_chain_count]);
    let minor_index = build_levels(&ordered_boxes[major_chain_count..]);
    let mut new_chain = vec![0u32; chain_order.len()];
    for (position, &chain) in chain_order.iter().enumerate() {
        new_chain[chain as usize] = position as u32;
    }
    let remap_turns = |entries: &[[u32; 2]]| -> Vec<[u32; 2]> {
        let mut remapped: Vec<[u32; 2]> =
            entries.iter().map(|&[key, node]| [turn_key(new_chain[(key / 2) as usize], key % 2 == 1), rank[node as usize]]).collect();
        remapped.sort_unstable();
        remapped
    };
    let turn_arrivals = remap_turns(&turns.arrivals);
    let turn_departures = remap_turns(&turns.departures);
    let mut node_coords = vec![Coord { lat: 0, lon: 0 }; topology.node_count()];
    for (node, &coord) in topology.node_coords.iter().enumerate() {
        node_coords[rank[node] as usize] = coord;
    }

    let mut writer = ArrayWriter::create(output)?;
    writer.write("ch_first_arc", &hierarchy.first_arc)?;
    writer.write("ch_arcs", &hierarchy.arcs)?;
    writer.write("chain_tail", &chain_tail)?;
    writer.write("chain_head", &chain_head)?;
    writer.write("chain_cost", &chain_cost)?;
    writer.write("chain_geometry_first", &geometry_first)?;
    writer.write("chain_geometry", &geometry)?;
    writer.write("node_coords", &node_coords)?;
    writer.write("turn_arrivals", &turn_arrivals)?;
    writer.write("turn_departures", &turn_departures)?;
    writer.write("major_index", &major_index)?;
    writer.write("minor_index", &minor_index)?;
    writer.finish(Manifest {
        format_version: FORMAT_VERSION,
        profile: profile.to_string(),
        source,
        built_at_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
        node_count: topology.node_count() as u32,
        ch_arc_count: hierarchy.arcs.len() as u64,
        chain_count: chain_order.len() as u32,
        major_chain_count: major_chain_count as u32,
        geometry_point_count: geometry.len() as u64,
        arrays: Vec::new(),
    })
}
