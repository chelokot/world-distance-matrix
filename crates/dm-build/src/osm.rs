use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use dm_core::geo::Coord;
use osmpbf::{BlobDecode, BlobReader, PrimitiveBlock, RelMemberType};
use rayon::prelude::*;

use crate::profile::{node_traits, turn_rule, way_profile, Tags, TurnRule, WayProfile};

pub struct Ways {
    pub profiles: Vec<WayProfile>,
    pub first_ref: Vec<u64>,
    pub refs: Vec<u32>,
}

impl Ways {
    pub fn count(&self) -> usize {
        self.profiles.len()
    }

    pub fn refs(&self, way: usize) -> &[u32] {
        &self.refs[self.first_ref[way] as usize..self.first_ref[way + 1] as usize]
    }
}

pub const NODE_PRESENT: u8 = 1;
pub const NODE_BLOCKS_CARS: u8 = 2;
pub const NODE_TRAFFIC_SIGNAL: u8 = 4;

pub struct Nodes {
    pub coords: Vec<Coord>,
    pub flags: Vec<u8>,
}

pub struct Restriction {
    pub rule: TurnRule,
    pub from: Vec<u32>,
    pub via: u32,
    pub to: Vec<u32>,
}

pub struct OsmExtract {
    pub ways: Ways,
    pub nodes: Nodes,
    pub restrictions: Vec<Restriction>,
    pub replication_timestamp: Option<i64>,
}

struct RawRestriction {
    rule: TurnRule,
    from: Vec<i64>,
    via: i64,
    to: Vec<i64>,
}

struct WayBatch {
    ids: Vec<i64>,
    profiles: Vec<WayProfile>,
    ref_counts: Vec<u32>,
    refs: Vec<i64>,
    restrictions: Vec<RawRestriction>,
}

fn raw_restriction(relation: &osmpbf::Relation) -> Option<RawRestriction> {
    let rule = turn_rule(&Tags::collect(relation.tags()))?;
    let (mut from, mut via, mut to) = (Vec::new(), None, Vec::new());
    for member in relation.members() {
        match (member.role().ok()?, member.member_type) {
            ("from", RelMemberType::Way) => from.push(member.member_id),
            ("to", RelMemberType::Way) => to.push(member.member_id),
            ("via", RelMemberType::Node) if via.is_none() => via = Some(member.member_id),
            ("via", _) => return None,
            _ => {}
        }
    }
    (!from.is_empty() && !to.is_empty()).then_some(RawRestriction { rule, from, via: via?, to })
}

fn for_each_block<T: Send>(path: &Path, visit: impl Fn(PrimitiveBlock) -> T + Sync + Send) -> Result<Vec<T>> {
    let reader = BlobReader::from_path(path).with_context(|| format!("opening {}", path.display()))?;
    let mut results: Vec<(usize, T)> = reader
        .enumerate()
        .par_bridge()
        .filter_map(|(index, blob)| {
            let blob = match blob {
                Ok(blob) => blob,
                Err(error) => return Some(Err(error)),
            };
            match blob.decode() {
                Ok(BlobDecode::OsmData(block)) => Some(Ok((index, visit(block)))),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect::<Result<_, _>>()
        .with_context(|| format!("reading {}", path.display()))?;
    results.sort_unstable_by_key(|(index, _)| *index);
    Ok(results.into_iter().map(|(_, value)| value).collect())
}

fn replication_timestamp(path: &Path) -> Result<Option<i64>> {
    let mut reader = BlobReader::from_path(path)?;
    let Some(blob) = reader.next().transpose()? else { return Ok(None) };
    match blob.decode()? {
        BlobDecode::OsmHeader(header) => Ok(header.osmosis_replication_timestamp()),
        _ => Ok(None),
    }
}

struct RawWays {
    ids: Vec<i64>,
    profiles: Vec<WayProfile>,
    first_ref: Vec<u64>,
    refs: Vec<i64>,
    restrictions: Vec<RawRestriction>,
}

fn read_ways(path: &Path) -> Result<RawWays> {
    let batches = for_each_block(path, |block| {
        let mut batch = WayBatch { ids: Vec::new(), profiles: Vec::new(), ref_counts: Vec::new(), refs: Vec::new(), restrictions: Vec::new() };
        for group in block.groups() {
            for way in group.ways() {
                let Some(profile) = way_profile(&Tags::collect(way.tags())) else { continue };
                let before = batch.refs.len();
                batch.refs.extend(way.refs());
                if batch.refs.len() - before < 2 {
                    batch.refs.truncate(before);
                    continue;
                }
                batch.ids.push(way.id());
                batch.profiles.push(profile);
                batch.ref_counts.push((batch.refs.len() - before) as u32);
            }
            batch.restrictions.extend(group.relations().filter_map(|relation| raw_restriction(&relation)));
        }
        batch
    })?;
    let way_count = batches.iter().map(|b| b.profiles.len()).sum();
    let ref_count = batches.iter().map(|b| b.refs.len()).sum();
    let mut ways = RawWays {
        ids: Vec::with_capacity(way_count),
        profiles: Vec::with_capacity(way_count),
        first_ref: Vec::with_capacity(way_count + 1),
        refs: Vec::with_capacity(ref_count),
        restrictions: Vec::new(),
    };
    ways.first_ref.push(0u64);
    for batch in batches {
        ways.ids.extend(batch.ids);
        ways.profiles.extend(batch.profiles);
        for count in batch.ref_counts {
            ways.first_ref.push(ways.first_ref.last().expect("starts with zero") + count as u64);
        }
        ways.refs.extend(batch.refs);
        ways.restrictions.extend(batch.restrictions);
    }
    Ok(ways)
}

fn resolve_restrictions(raw: Vec<RawRestriction>, way_ids: &[i64], node_ids: &[i64]) -> Vec<Restriction> {
    let mut way_index: Vec<(i64, u32)> = way_ids.iter().enumerate().map(|(index, &id)| (id, index as u32)).collect();
    way_index.par_sort_unstable();
    let ways_of = |ids: &[i64]| -> Vec<u32> {
        ids.iter().filter_map(|id| way_index.binary_search_by_key(id, |&(known, _)| known).ok().map(|slot| way_index[slot].1)).collect()
    };
    raw.into_iter()
        .filter_map(|r| {
            let via = node_ids.binary_search(&r.via).ok()? as u32;
            let (from, to) = (ways_of(&r.from), ways_of(&r.to));
            (!from.is_empty() && !to.is_empty()).then_some(Restriction { rule: r.rule, from, via, to })
        })
        .collect()
}

struct NodeScan<'a> {
    ids: &'a [i64],
    cursor: usize,
    found: Vec<(u32, Coord, u8)>,
}

impl NodeScan<'_> {
    fn visit<'t>(&mut self, id: i64, coord: Coord, tags: impl Iterator<Item = (&'t str, &'t str)>) {
        let ids = self.ids;
        if self.cursor > 0 && ids[self.cursor - 1] >= id || self.cursor < ids.len() && ids[self.cursor] < id {
            self.cursor = ids.partition_point(|&known| known < id);
        }
        if self.cursor < ids.len() && ids[self.cursor] == id {
            let traits = node_traits(&Tags::collect(tags));
            let flags = NODE_PRESENT | if traits.blocks_cars { NODE_BLOCKS_CARS } else { 0 } | if traits.traffic_signal { NODE_TRAFFIC_SIGNAL } else { 0 };
            self.found.push((self.cursor as u32, coord, flags));
            self.cursor += 1;
        }
    }
}

fn read_nodes(path: &Path, ids: &[i64]) -> Result<Nodes> {
    let nodes = Mutex::new(Nodes { coords: vec![Coord { lat: 0, lon: 0 }; ids.len()], flags: vec![0u8; ids.len()] });
    for_each_block(path, |block| {
        let mut scan = NodeScan { ids, cursor: 0, found: Vec::new() };
        for group in block.groups() {
            for node in group.dense_nodes() {
                scan.visit(node.id(), Coord { lat: node.decimicro_lat(), lon: node.decimicro_lon() }, node.tags());
            }
            for node in group.nodes() {
                scan.visit(node.id(), Coord { lat: node.decimicro_lat(), lon: node.decimicro_lon() }, node.tags());
            }
        }
        let mut nodes = nodes.lock().expect("node table lock");
        for (index, coord, flags) in scan.found {
            nodes.coords[index as usize] = coord;
            nodes.flags[index as usize] = flags;
        }
    })?;
    Ok(nodes.into_inner().expect("node table lock"))
}

pub fn read(path: &Path) -> Result<OsmExtract> {
    let replication_timestamp = replication_timestamp(path)?;
    let started = std::time::Instant::now();
    let raw = read_ways(path)?;
    tracing::info!(
        ways = raw.profiles.len(),
        refs = raw.refs.len(),
        restrictions = raw.restrictions.len(),
        elapsed_s = started.elapsed().as_secs_f32(),
        "read routable ways and turn restrictions"
    );
    let mut ids = raw.refs.clone();
    ids.par_sort_unstable();
    ids.dedup();
    let refs: Vec<u32> = raw.refs.par_iter().map(|id| ids.binary_search(id).expect("every ref is in the id set") as u32).collect();
    let restrictions = resolve_restrictions(raw.restrictions, &raw.ids, &ids);
    drop(raw.refs);
    let started = std::time::Instant::now();
    let nodes = read_nodes(path, &ids)?;
    let present = nodes.flags.par_iter().filter(|&&f| f & NODE_PRESENT != 0).count();
    tracing::info!(referenced = ids.len(), present, restrictions = restrictions.len(), elapsed_s = started.elapsed().as_secs_f32(), "read referenced nodes");
    Ok(OsmExtract { ways: Ways { profiles: raw.profiles, first_ref: raw.first_ref, refs }, nodes, restrictions, replication_timestamp })
}
