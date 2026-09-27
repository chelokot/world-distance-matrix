pub mod assemble;
pub mod components;
pub mod contract;
pub mod osm;
pub mod profile;
pub mod topology;
pub mod turns;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
pub struct BuildConfig {
    pub input: PathBuf,
    pub output: PathBuf,
    pub witness_settle_limit: usize,
    pub major_component_min_nodes: u32,
    pub simplify_tolerance_m: f64,
    pub today: profile::Day,
}

fn peak_memory_gib() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| status.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|kb| kb.parse::<f64>().ok()))
        .map_or(0.0, |kb| kb / (1024.0 * 1024.0))
}

pub fn build(args: &BuildConfig) -> Result<()> {
    let started = Instant::now();

    let extract = osm::read(&args.input, args.today)?;
    let mut topology = topology::build(&extract);
    let source = format!(
        "{} (replication timestamp {})",
        args.input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        extract.replication_timestamp.map_or_else(|| "unknown".to_string(), |t| t.to_string())
    );
    drop(extract);
    tracing::info!(
        nodes = topology.node_count(),
        chains = topology.chains.len(),
        interior = topology.interior.len(),
        elapsed_s = started.elapsed().as_secs_f32(),
        peak_gib = peak_memory_gib(),
        "built topology"
    );
    topology.simplify_geometry(args.simplify_tolerance_m);
    tracing::info!(interior = topology.interior.len(), elapsed_s = started.elapsed().as_secs_f32(), peak_gib = peak_memory_gib(), "simplified geometry");

    let (turns, turn_stats) = turns::split_restricted_junctions(&mut topology);
    tracing::info!(
        restrictions = topology.restrictions.len(),
        applied = turn_stats.applied,
        skipped = turn_stats.skipped,
        junction_copies = turn_stats.junction_copies,
        "split junctions with turn restrictions"
    );
    let arcs = turns::arcs(&topology, &turns);
    let components = components::strongly_connected(topology.node_count(), arcs.clone());
    let major_nodes: u64 = components.sizes.iter().filter(|&&s| s >= args.major_component_min_nodes).map(|&s| s as u64).sum();
    tracing::info!(
        components = components.sizes.len(),
        largest = components.sizes.iter().max().copied().unwrap_or(0),
        major_nodes,
        elapsed_s = started.elapsed().as_secs_f32(),
        peak_gib = peak_memory_gib(),
        "computed strongly connected components"
    );

    let hierarchy = contract::contract(&topology.node_coords, arcs, &contract::Params { witness_settle_limit: args.witness_settle_limit });
    tracing::info!(arcs = hierarchy.arcs.len(), elapsed_s = started.elapsed().as_secs_f32(), peak_gib = peak_memory_gib(), "built contraction hierarchy");

    assemble::write(
        &args.output,
        source,
        profile::PROFILE_NAME,
        &assemble::Built { topology: &topology, turns: &turns, components: &components, hierarchy: &hierarchy },
        &assemble::AssemblyParams { major_component_min_nodes: args.major_component_min_nodes },
    )?;
    tracing::info!(output = %args.output.display(), elapsed_s = started.elapsed().as_secs_f32(), peak_gib = peak_memory_gib(), "dataset written");
    Ok(())
}
