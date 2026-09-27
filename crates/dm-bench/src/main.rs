mod compare;
mod http;
mod points;
mod verify;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};
use dm_core::geo::Coord;
use dm_core::matrix::{Endpoint, MatrixJob};
use dm_core::network::Network;
use dm_core::snap::{snap, SnapConfig};
use dm_core::store::Residency;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rayon::prelude::*;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    DatasetPoints {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        bbox: String,
        #[arg(long, default_value_t = 50_000)]
        count: usize,
    },
    Points {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        bbox: Option<String>,
        #[arg(long, default_value_t = 200_000)]
        count: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    Verify {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        points: PathBuf,
        #[arg(long, default_value_t = 400)]
        size: usize,
        #[arg(long, default_value_t = 60)]
        reference_sources: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    Http {
        #[arg(long)]
        url: String,
        #[arg(long = "scenario", value_parser = parse_scenario, required = true)]
        scenarios: Vec<(String, PathBuf)>,
        #[arg(long, value_delimiter = ',', default_value = "10,100,1000")]
        sizes: Vec<usize>,
        #[arg(long, value_enum, default_value = "binary")]
        format: http::WireFormat,
        #[arg(long, value_enum, default_value = "square")]
        shape: http::Shape,
        #[arg(long, default_value_t = 100)]
        requests: usize,
        #[arg(long, default_value_t = 5)]
        warmup: usize,
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    CompareOsrm {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        osrm_url: String,
        #[arg(long)]
        points: PathBuf,
        #[arg(long, default_value_t = 300)]
        size: usize,
        #[arg(long, default_value_t = 3)]
        rounds: usize,
    },
    Path {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    Profile {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        points: PathBuf,
        #[arg(long, default_value_t = 1000)]
        size: usize,
    },
    Engine {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        points: PathBuf,
        #[arg(long, value_delimiter = ',', default_value = "10,100,1000")]
        sizes: Vec<usize>,
        #[arg(long, default_value_t = 20)]
        repeats: usize,
    },
}

fn parse_scenario(value: &str) -> Result<(String, PathBuf), String> {
    let (label, path) = value.split_once('=').ok_or("expected label=path")?;
    Ok((label.to_string(), PathBuf::from(path)))
}

fn sample(pool: &[Coord], size: usize, seed: u64) -> Vec<Coord> {
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    pool.choose_multiple(&mut rng, size).copied().collect()
}

fn endpoints(network: &Network, coords: &[Coord]) -> Vec<Endpoint> {
    let config = SnapConfig::default();
    coords.par_iter().map(|&coord| Endpoint { coord, placement: snap(network, coord, &config).map(|s| s.placement) }).collect()
}

fn percentile(sorted: &[Duration], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize].as_secs_f64() * 1e3
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::DatasetPoints { data, output, bbox, count } => {
            let network = Network::open(&data, Residency::OnDemand)?;
            let region = points::Region::parse(&bbox)?;
            let mut inside: Vec<Coord> = network.node_coords.iter().copied().filter(|&c| region.contains(c)).collect();
            inside.shuffle(&mut rand::rngs::StdRng::seed_from_u64(9));
            inside.truncate(count);
            points::write_csv(&output, &inside)?;
            println!("wrote {} junction points to {}", inside.len(), output.display());
        }
        Command::Points { input, output, bbox, count, seed } => {
            let region = bbox.as_deref().map(points::Region::parse).transpose()?;
            let found = points::extract(&input, region, count, seed)?;
            points::write_csv(&output, &found)?;
            println!("wrote {} points to {}", found.len(), output.display());
        }
        Command::Verify { data, points, size, reference_sources, seed } => {
            let network = Network::open(&data, Residency::Prefault)?;
            let pool = points::read_csv(&points)?;
            let coords = sample(&pool, size, seed);
            let mut endpoints = endpoints(&network, &coords);
            endpoints.push(endpoints[0]);
            let report = verify::verify(&network, &endpoints, reference_sources);
            println!("{report:#?}");
        }
        Command::Http { url, scenarios, sizes, format, shape, requests, warmup, concurrency, output } => {
            let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
            let mut summaries = Vec::new();
            println!(
                "{:<20} {:>6} {:>4} {:>10} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8}",
                "scenario", "size", "conc", "bytes", "ttlb_p50", "ttlb_p90", "ttlb_p99", "ttlb_max", "dec_p50", "tot_p99", "rps"
            );
            for (label, path) in &scenarios {
                let pool = points::read_csv(path)?;
                for &size in &sizes {
                    let bodies: Vec<String> = (0..requests.max(warmup)).map(|i| http::request_body(&sample(&pool, size, 7_000 + i as u64), shape)).collect();
                    let plan = http::Plan { url: url.clone(), label: label.clone(), format, shape, size, requests, warmup, concurrency };
                    let summary = runtime.block_on(http::run(&plan, std::sync::Arc::new(bodies)))?;
                    println!(
                        "{:<20} {:>6} {:>4} {:>10} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>8.1}",
                        summary.label,
                        summary.size,
                        summary.concurrency,
                        summary.bytes,
                        summary.ttlb_p50_ms,
                        summary.ttlb_p90_ms,
                        summary.ttlb_p99_ms,
                        summary.ttlb_max_ms,
                        summary.decode_p50_ms,
                        summary.total_p99_ms,
                        summary.throughput_rps
                    );
                    summaries.push(summary);
                }
            }
            if let Some(output) = output {
                let mut all: Vec<serde_json::Value> = std::fs::read(&output).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
                all.extend(summaries.iter().map(|s| serde_json::to_value(s).expect("summaries serialize")));
                std::fs::write(&output, serde_json::to_vec_pretty(&all)?)?;
            }
        }
        Command::CompareOsrm { data, osrm_url, points, size, rounds } => {
            let network = Network::open(&data, Residency::Prefault)?;
            let pool = points::read_csv(&points)?;
            let mut comparison = compare::Comparison::default();
            for round in 0..rounds {
                let coords = sample(&pool, size, 500 + round as u64);
                let ours = verify::engine_matrix(&network, &endpoints(&network, &coords));
                let reference = compare::osrm_table(&osrm_url, &coords)?;
                comparison.add(&ours, &reference);
                if round == 0 {
                    compare::worst(&coords, &ours, &reference, 12);
                }
            }
            comparison.print();
        }
        Command::Path { data, from, to } => {
            let network = Network::open(&data, Residency::Prefault)?;
            let parse = |text: &str| -> Result<Coord> {
                let (lat, lon) = text.split_once(',').ok_or_else(|| anyhow::anyhow!("expected lat,lon"))?;
                Ok(Coord::from_degrees(lat.trim().parse()?, lon.trim().parse()?))
            };
            let points = endpoints(&network, &[parse(&from)?, parse(&to)?]);
            for (lat, lon) in verify::path_polyline(&network, points[0], points[1]) {
                println!("{lat:.6},{lon:.6}");
            }
        }
        Command::Profile { data, points, size } => {
            use dm_core::search::{Direction, UpwardSearch};
            let network = Network::open(&data, Residency::Prefault)?;
            let pool = points::read_csv(&points)?;
            let endpoints = endpoints(&network, &sample(&pool, size, 42));
            let seeds = |e: &Endpoint, forward: bool| -> Vec<(u32, u64)> {
                let Some(p) = e.placement else { return Vec::new() };
                let c = p.chain as usize;
                let cost = network.chains.cost[c];
                let (t, h) = (network.chains.tail[c], network.chains.head[c]);
                let (a, b) = if forward {
                    (cost.forward().map(|w| (h, w)), cost.backward().map(|w| (t, w)))
                } else {
                    (cost.forward().map(|w| (t, w)), cost.backward().map(|w| (h, w)))
                };
                a.into_iter().chain(b).collect()
            };
            let mut search = UpwardSearch::default();
            let mut bucket_size: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
            let mut backward_total = 0usize;
            for e in &endpoints {
                let space = search.run(&network.hierarchy, &seeds(e, false), Direction::Backward);
                backward_total += space.len();
                for &(v, _) in space {
                    *bucket_size.entry(v).or_default() += 1;
                }
            }
            let mut forward_total = 0usize;
            let mut scanned = 0usize;
            let mut scanned_dense = [0usize; 4];
            let mut visits_dense = [0usize; 4];
            for e in &endpoints {
                let space = search.run(&network.hierarchy, &seeds(e, true), Direction::Forward);
                forward_total += space.len();
                for &(v, _) in space {
                    let b = bucket_size.get(&v).copied().unwrap_or(0);
                    scanned += b;
                    for (k, threshold) in [size / 16, size / 8, size / 4, size / 2].iter().enumerate() {
                        if b >= *threshold {
                            scanned_dense[k] += b;
                            visits_dense[k] += 1;
                        }
                    }
                }
            }
            let n = endpoints.len() as f64;
            println!(
                "points {size}: avg backward space {:.0}, avg forward space {:.0}, bucket entries {}, distinct bucket nodes {}",
                backward_total as f64 / n,
                forward_total as f64 / n,
                backward_total,
                bucket_size.len()
            );
            println!("scanned entries {} ({:.1} per cell)", scanned, scanned as f64 / (n * n));
            for (k, label) in ["N/16", "N/8", "N/4", "N/2"].iter().enumerate() {
                let dense_nodes = bucket_size.values().filter(|&&b| b >= [size / 16, size / 8, size / 4, size / 2][k]).count();
                println!(
                    "  buckets >= {label}: {dense_nodes} nodes, {:.1}% of scanned entries, {:.0} visits per source",
                    100.0 * scanned_dense[k] as f64 / scanned as f64,
                    visits_dense[k] as f64 / n
                );
            }
        }
        Command::Engine { data, points, sizes, repeats } => {
            let started = Instant::now();
            let network = Network::open(&data, Residency::Prefault)?;
            println!("loaded {} nodes in {:.1}s", network.manifest.node_count, started.elapsed().as_secs_f32());
            let pool = points::read_csv(&points)?;
            println!("{:>6} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}", "size", "snap_p50", "prep_p50", "rows_p50", "total_p50", "total_p90", "total_p99");
            for size in sizes {
                let mut phases: [Vec<Duration>; 4] = Default::default();
                for repeat in 0..repeats {
                    let coords = sample(&pool, size, repeat as u64 + 1000);
                    let t0 = Instant::now();
                    let endpoints = endpoints(&network, &coords);
                    let t1 = Instant::now();
                    let job = MatrixJob::prepare(&network, endpoints.clone(), endpoints);
                    let t2 = Instant::now();
                    let mut out = vec![0u32; size * size * 2];
                    job.compute_rows(0..size, &mut out);
                    let t3 = Instant::now();
                    for (phase, d) in phases.iter_mut().zip([t1 - t0, t2 - t1, t3 - t2, t3 - t0]) {
                        phase.push(d);
                    }
                }
                phases.iter_mut().for_each(|p| p.sort());
                println!(
                    "{size:>6} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>9.2}",
                    percentile(&phases[0], 0.5),
                    percentile(&phases[1], 0.5),
                    percentile(&phases[2], 0.5),
                    percentile(&phases[3], 0.5),
                    percentile(&phases[3], 0.9),
                    percentile(&phases[3], 0.99)
                );
            }
        }
    }
    Ok(())
}
