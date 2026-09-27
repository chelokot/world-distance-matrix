use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use dm_build::{build, BuildConfig};

#[derive(Parser)]
#[command(about = "Builds a routing dataset for the distance matrix service from an OpenStreetMap PBF extract")]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 500)]
    witness_settle_limit: usize,
    #[arg(long, default_value_t = 1000)]
    major_component_min_nodes: u32,
    #[arg(long, default_value_t = 5.0)]
    simplify_tolerance_m: f64,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let args = Args::parse();
    build(&BuildConfig {
        input: args.input,
        output: args.output,
        witness_settle_limit: args.witness_settle_limit,
        major_component_min_nodes: args.major_component_min_nodes,
        simplify_tolerance_m: args.simplify_tolerance_m,
    })
}
