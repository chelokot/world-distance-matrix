use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use dm_core::geo::Coord;
use dm_core::wire::{decode_binary, BINARY_CONTENT_TYPE};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum WireFormat {
    Binary,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    Square,
    Row,
    Column,
}

impl Shape {
    pub fn cells(self, size: usize) -> usize {
        match self {
            Shape::Square => size * size,
            Shape::Row | Shape::Column => size,
        }
    }
}

#[derive(Serialize)]
pub struct Sample {
    pub size: usize,
    pub bytes: usize,
    pub ttfb_ms: f64,
    pub ttlb_ms: f64,
    pub decode_ms: f64,
}

#[derive(Deserialize)]
struct JsonMatrix {
    distances: Vec<Vec<Option<u32>>>,
    times: Vec<Vec<Option<u32>>>,
}

pub fn request_body(coords: &[Coord], shape: Shape) -> String {
    let locations: Vec<serde_json::Value> = coords.iter().map(|c| serde_json::json!({ "lat": c.lat_degrees(), "lon": c.lon_degrees() })).collect();
    match shape {
        Shape::Square => serde_json::json!({ "coordinates": locations }),
        Shape::Row => serde_json::json!({ "coordinates": locations, "sources": [0] }),
        Shape::Column => serde_json::json!({ "coordinates": locations, "destinations": [0] }),
    }
    .to_string()
}

pub async fn measure(client: &reqwest::Client, url: &str, format: WireFormat, body: String, size: usize, cells: usize) -> Result<Sample> {
    let accept = match format {
        WireFormat::Binary => BINARY_CONTENT_TYPE,
        WireFormat::Json => "application/json",
    };
    let started = Instant::now();
    let response = client.post(url).header("content-type", "application/json").header("accept", accept).body(body).send().await?;
    let ttfb = started.elapsed();
    ensure!(response.status().is_success(), "server answered {}", response.status());
    let bytes = response.bytes().await?;
    let ttlb = started.elapsed();
    let decoded_cells = match format {
        WireFormat::Binary => {
            let matrix = decode_binary(&bytes)?;
            matrix.distances.len() + matrix.durations.len()
        }
        WireFormat::Json => {
            let matrix: JsonMatrix = serde_json::from_slice(&bytes).context("decoding JSON matrix")?;
            matrix.distances.iter().map(Vec::len).sum::<usize>() + matrix.times.iter().map(Vec::len).sum::<usize>()
        }
    };
    let finished = started.elapsed();
    ensure!(decoded_cells == 2 * cells, "expected {cells} cells");
    Ok(Sample {
        size,
        bytes: bytes.len(),
        ttfb_ms: ttfb.as_secs_f64() * 1e3,
        ttlb_ms: ttlb.as_secs_f64() * 1e3,
        decode_ms: (finished - ttlb).as_secs_f64() * 1e3,
    })
}

#[derive(Serialize)]
pub struct Summary {
    pub label: String,
    pub format: WireFormat,
    pub shape: Shape,
    pub size: usize,
    pub concurrency: usize,
    pub requests: usize,
    pub bytes: usize,
    pub ttlb_p50_ms: f64,
    pub ttlb_p90_ms: f64,
    pub ttlb_p99_ms: f64,
    pub ttlb_max_ms: f64,
    pub decode_p50_ms: f64,
    pub total_p50_ms: f64,
    pub total_p99_ms: f64,
    pub throughput_rps: f64,
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).round() as usize]
}

pub fn summarize(label: &str, format: WireFormat, shape: Shape, size: usize, concurrency: usize, samples: &[Sample], wall: Duration) -> Summary {
    let mut ttlb: Vec<f64> = samples.iter().map(|s| s.ttlb_ms).collect();
    let mut decode: Vec<f64> = samples.iter().map(|s| s.decode_ms).collect();
    let mut total: Vec<f64> = samples.iter().map(|s| s.ttlb_ms + s.decode_ms).collect();
    Summary {
        label: label.to_string(),
        format,
        shape,
        size,
        concurrency,
        requests: samples.len(),
        bytes: samples.first().map_or(0, |s| s.bytes),
        ttlb_p50_ms: percentile(&mut ttlb, 0.5),
        ttlb_p90_ms: percentile(&mut ttlb, 0.9),
        ttlb_p99_ms: percentile(&mut ttlb, 0.99),
        ttlb_max_ms: percentile(&mut ttlb, 1.0),
        decode_p50_ms: percentile(&mut decode, 0.5),
        total_p50_ms: percentile(&mut total, 0.5),
        total_p99_ms: percentile(&mut total, 0.99),
        throughput_rps: samples.len() as f64 / wall.as_secs_f64(),
    }
}

pub struct Plan {
    pub url: String,
    pub label: String,
    pub format: WireFormat,
    pub shape: Shape,
    pub size: usize,
    pub requests: usize,
    pub warmup: usize,
    pub concurrency: usize,
}

pub async fn run(plan: &Plan, bodies: Arc<Vec<String>>) -> Result<Summary> {
    let client = reqwest::Client::builder().pool_max_idle_per_host(plan.concurrency).tcp_nodelay(true).build()?;
    for body in bodies.iter().take(plan.warmup) {
        measure(&client, &plan.url, plan.format, body.clone(), plan.size, plan.shape.cells(plan.size)).await?;
    }
    let started = Instant::now();
    let per_worker = plan.requests.div_ceil(plan.concurrency);
    let workers: Vec<_> = (0..plan.concurrency)
        .map(|worker| {
            let client = client.clone();
            let bodies = Arc::clone(&bodies);
            let (url, format, size, cells) = (plan.url.clone(), plan.format, plan.size, plan.shape.cells(plan.size));
            tokio::spawn(async move {
                let mut samples = Vec::with_capacity(per_worker);
                for i in 0..per_worker {
                    let body = bodies[(worker * per_worker + i) % bodies.len()].clone();
                    samples.push(measure(&client, &url, format, body, size, cells).await?);
                }
                anyhow::Ok(samples)
            })
        })
        .collect();
    let mut samples = Vec::new();
    for worker in workers {
        samples.extend(worker.await??);
    }
    Ok(summarize(&plan.label, plan.format, plan.shape, plan.size, plan.concurrency, &samples, started.elapsed()))
}
