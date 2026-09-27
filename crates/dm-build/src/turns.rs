use std::collections::{BTreeMap, BTreeSet};

use dm_core::network::{turn_key, Turns};
use dm_core::weight::Weight;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::profile::TurnRule;
use crate::topology::Topology;

fn arrivals_at(topology: &Topology, node: u32, chains: &[u32]) -> Vec<(u32, u32)> {
    chains
        .iter()
        .flat_map(|&c| {
            let chain = &topology.chains[c as usize];
            let forward = (chain.head == node && chain.cost.forward().is_some()).then(|| (turn_key(c, false), topology.chain_way[c as usize]));
            let backward = (chain.tail == node && chain.cost.backward().is_some()).then(|| (turn_key(c, true), topology.chain_way[c as usize]));
            forward.into_iter().chain(backward)
        })
        .collect()
}

fn departures_at(topology: &Topology, node: u32, chains: &[u32]) -> Vec<(u32, u32)> {
    chains
        .iter()
        .flat_map(|&c| {
            let chain = &topology.chains[c as usize];
            let forward = (chain.tail == node && chain.cost.forward().is_some()).then(|| (turn_key(c, false), topology.chain_way[c as usize]));
            let backward = (chain.head == node && chain.cost.backward().is_some()).then(|| (turn_key(c, true), topology.chain_way[c as usize]));
            forward.into_iter().chain(backward)
        })
        .collect()
}

pub struct TurnStats {
    pub applied: usize,
    pub skipped: usize,
    pub junction_copies: usize,
}

pub fn split_restricted_junctions(topology: &mut Topology) -> (Turns, TurnStats) {
    let vias: FxHashSet<u32> = topology.restrictions.iter().map(|r| r.via).collect();
    let mut incident: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    for (index, chain) in topology.chains.iter().enumerate() {
        for node in [chain.tail, chain.head] {
            if vias.contains(&node) {
                incident.entry(node).or_default().push(index as u32);
            }
        }
    }
    let mut banned: BTreeMap<(u32, u32), BTreeSet<u32>> = BTreeMap::new();
    let mut stats = TurnStats { applied: 0, skipped: 0, junction_copies: 0 };
    for restriction in &topology.restrictions {
        let chains = &incident[&restriction.via];
        let arrivals: Vec<u32> =
            arrivals_at(topology, restriction.via, chains).into_iter().filter(|(_, way)| restriction.from.contains(way)).map(|(key, _)| key).collect();
        let departures = departures_at(topology, restriction.via, chains);
        let targeted: BTreeSet<u32> = departures.iter().filter(|(_, way)| restriction.to.contains(way)).map(|&(key, _)| key).collect();
        if arrivals.is_empty() || targeted.is_empty() {
            stats.skipped += 1;
            continue;
        }
        stats.applied += 1;
        let forbidden: Vec<u32> = match restriction.rule {
            TurnRule::Forbid => targeted.into_iter().collect(),
            TurnRule::Only => departures.iter().map(|&(key, _)| key).filter(|key| !targeted.contains(key)).collect(),
        };
        for arrival in arrivals {
            banned.entry((restriction.via, arrival)).or_default().extend(forbidden.iter().copied());
        }
    }
    let mut arrivals = Vec::new();
    let mut departures = Vec::new();
    for ((via, arrival), forbidden) in banned {
        if forbidden.is_empty() {
            continue;
        }
        let copy = topology.node_coords.len() as u32;
        topology.node_coords.push(topology.node_coords[via as usize]);
        stats.junction_copies += 1;
        arrivals.push([arrival, copy]);
        for (departure, _) in departures_at(topology, via, &incident[&via]) {
            if !forbidden.contains(&departure) {
                departures.push([departure, copy]);
            }
        }
    }
    arrivals.sort_unstable();
    departures.sort_unstable();
    (Turns { arrivals: arrivals.into(), departures: departures.into() }, stats)
}

pub fn arcs<'a>(topology: &'a Topology, turns: &'a Turns) -> impl Iterator<Item = (u32, u32, Weight)> + Clone + 'a {
    topology.chains.iter().enumerate().flat_map(move |(index, chain)| {
        let index = index as u32;
        let directed = [(false, chain.cost.forward(), chain.tail, chain.head), (true, chain.cost.backward(), chain.head, chain.tail)];
        directed.into_iter().flat_map(move |(backward, weight, from, to)| {
            weight.into_iter().flat_map(move |weight| {
                let arrival = turns.arrival(index, backward).unwrap_or(to);
                std::iter::once(from).chain(turns.departures(index, backward)).map(move |departure| (departure, arrival, weight))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{Chain, GraphRestriction};
    use dm_core::geo::Coord;
    use dm_core::weight::{ChainCost, NOT_TRAVERSABLE};

    fn two_way(tail: u32, head: u32) -> Chain {
        Chain { tail, head, cost: ChainCost { dist_dm: 100, forward_time_ms: 1000, backward_time_ms: 1000 } }
    }

    fn crossroads(rule: TurnRule) -> Topology {
        Topology {
            node_coords: vec![Coord { lat: 0, lon: 0 }; 5],
            chains: vec![
                two_way(1, 0),
                two_way(0, 2),
                two_way(0, 3),
                Chain { tail: 0, head: 4, cost: ChainCost { dist_dm: 100, forward_time_ms: 1000, backward_time_ms: NOT_TRAVERSABLE } },
            ],
            chain_way: vec![10, 20, 30, 40],
            interior_first: vec![0; 5],
            interior: Vec::new(),
            restrictions: vec![GraphRestriction { rule, from: vec![10], via: 0, to: vec![20] }],
        }
    }

    fn exits_after_arriving(topology: &Topology, turns: &Turns, chain: u32, backward: bool, junction: u32) -> BTreeSet<u32> {
        let arrival = turns.arrival(chain, backward).unwrap_or(junction);
        arcs(topology, turns).filter(|&(from, _, _)| from == arrival).map(|(_, to, _)| to).collect()
    }

    #[test]
    fn forbidden_turn_is_removed_only_for_the_restricted_approach() {
        let mut topology = crossroads(TurnRule::Forbid);
        let (turns, stats) = split_restricted_junctions(&mut topology);
        assert_eq!((stats.applied, stats.junction_copies), (1, 1));
        assert_eq!(exits_after_arriving(&topology, &turns, 0, false, 0), BTreeSet::from([1, 3, 4]));
        assert_eq!(exits_after_arriving(&topology, &turns, 2, true, 0), BTreeSet::from([1, 2, 3, 4]));
    }

    #[test]
    fn only_turn_keeps_just_the_mandated_exit() {
        let mut topology = crossroads(TurnRule::Only);
        let (turns, _) = split_restricted_junctions(&mut topology);
        assert_eq!(exits_after_arriving(&topology, &turns, 0, false, 0), BTreeSet::from([2]));
    }

    #[test]
    fn restrictions_with_unknown_members_are_skipped() {
        let mut topology = crossroads(TurnRule::Forbid);
        topology.restrictions[0].to = vec![99];
        let (turns, stats) = split_restricted_junctions(&mut topology);
        assert_eq!((stats.applied, stats.skipped, turns.arrivals.len()), (0, 1, 0));
    }
}
