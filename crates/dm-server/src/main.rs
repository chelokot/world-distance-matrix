mod api;
mod engine;
mod metrics;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::serve::ListenerExt;
use axum::Router;
use clap::Parser;
use dm_core::compact::COMPACT_CONTENT_TYPE;
use dm_core::network::Network;
use dm_core::snap::SnapConfig;
use dm_core::store::Residency;
use dm_core::wire::BINARY_CONTENT_TYPE;
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::compression::{CompressionLayer, CompressionLevel};

use crate::api::{negotiate, ApiError, Limits, MatrixRequest};
use crate::engine::{AdmissionConfig, Engine};
use crate::metrics::Metrics;

#[derive(Parser)]
#[command(about = "Road distance matrix service")]
struct Config {
    #[arg(long, env = "DM_DATA")]
    data: PathBuf,
    #[arg(long, env = "DM_LISTEN", default_value = "0.0.0.0:8080")]
    listen: SocketAddr,
    #[arg(long, env = "DM_THREADS")]
    threads: Option<usize>,
    #[arg(long, env = "DM_MAX_LOCATIONS", default_value_t = 25_000)]
    max_locations: usize,
    #[arg(long, env = "DM_MAX_CELLS", default_value_t = 100_000_000)]
    max_cells: usize,
    #[arg(long, env = "DM_MAX_JSON_CELLS", default_value_t = 16_000_000)]
    max_json_cells: usize,
    #[arg(long, env = "DM_CAPACITY_CELLS", default_value_t = 200_000_000)]
    capacity_cells: usize,
    #[arg(long, env = "DM_MAX_QUEUED", default_value_t = 256)]
    max_queued: usize,
    #[arg(long, env = "DM_QUEUE_TIMEOUT_MS", default_value_t = 10_000)]
    queue_timeout_ms: u64,
    #[arg(long, env = "DM_SNAP_MAX_DISTANCE_M", default_value_t = 5_000.0)]
    snap_max_distance_m: f64,
    #[arg(long, env = "DM_BLOCK_BYTES", default_value_t = 1 << 20)]
    block_bytes: usize,
    #[arg(long, env = "DM_LOCK_MEMORY", default_value_t = true, action = clap::ArgAction::Set)]
    lock_memory: bool,
    #[arg(long, env = "DM_LOG_JSON", default_value_t = true, action = clap::ArgAction::Set)]
    log_json: bool,
}

struct AppState {
    engine: Arc<Engine>,
    limits: Limits,
    metrics: Arc<Metrics>,
}

async fn matrix(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let started = Instant::now();
    let format = negotiate(&headers);
    let label = format.as_ref().map_or("unknown", |f| f.label());
    let outcome = async {
        let format = format?;
        let request: MatrixRequest = serde_json::from_slice(&body).map_err(|e| ApiError::BadRequest(format!("invalid request body: {e}")))?;
        let spec = request.validate(format, &state.limits)?;
        let shape = (spec.sources.len(), spec.destinations.len());
        state.engine.matrix(spec).await.map(|response| (response, shape))
    }
    .await;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1e3;
    match outcome {
        Ok((response, (rows, cols))) => {
            state.metrics.requests.with_label_values(&["200", label]).inc();
            tracing::debug!(rows, cols, format = label, time_to_headers_ms = elapsed_ms, "matrix accepted");
            response
        }
        Err(error) => {
            state.metrics.requests.with_label_values(&[error.status().as_str(), label]).inc();
            tracing::warn!(status = error.status().as_u16(), %error, format = label, elapsed_ms, "matrix rejected");
            error.into_response()
        }
    }
}

async fn health(State(state): State<Arc<AppState>>) -> Response {
    let manifest = &state.engine.network().manifest;
    let body = serde_json::json!({
        "status": "ok",
        "dataset": { "source": manifest.source, "profile": manifest.profile, "built_at_unix": manifest.built_at_unix, "nodes": manifest.node_count },
    });
    ([(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], state.metrics.render()).into_response()
}

async fn shutdown_signal() {
    let interrupt = async { tokio::signal::ctrl_c().await.expect("installing the Ctrl-C handler") };
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("installing the SIGTERM handler").recv().await;
    };
    tokio::select! {
        () = interrupt => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown requested, draining in-flight requests");
}

fn router(state: Arc<AppState>, max_locations: usize) -> Router {
    Router::new()
        .route("/matrix", post(matrix))
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .layer(DefaultBodyLimit::max(max_locations * 256 + 65_536))
        .layer(
            CompressionLayer::new()
                .quality(CompressionLevel::Fastest)
                .compress_when(DefaultPredicate::new().and(NotForContentType::new(BINARY_CONTENT_TYPE)).and(NotForContentType::new(COMPACT_CONTENT_TYPE))),
        )
        .with_state(state)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    let subscriber = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()));
    if config.log_json {
        subscriber.json().flatten_event(true).init();
    } else {
        subscriber.init();
    }
    let started = Instant::now();
    let residency = if config.lock_memory { Residency::Locked } else { Residency::Prefault };
    let network = Network::open(&config.data, residency).with_context(|| format!("loading dataset from {}", config.data.display()))?;
    tracing::info!(source = %network.manifest.source, nodes = network.manifest.node_count, elapsed_s = started.elapsed().as_secs_f32(), "dataset loaded");
    let metrics = Arc::new(Metrics::new()?);
    let threads = config.threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    let engine = Arc::new(Engine::new(
        network,
        threads,
        SnapConfig { max_distance_m: config.snap_max_distance_m, ..SnapConfig::default() },
        config.block_bytes,
        AdmissionConfig { capacity_cells: config.capacity_cells, max_queued: config.max_queued, queue_timeout: Duration::from_millis(config.queue_timeout_ms) },
        Arc::clone(&metrics),
    )?);
    let limits = Limits { max_locations: config.max_locations, max_cells: config.max_cells, max_json_cells: config.max_json_cells };
    let state = Arc::new(AppState { engine, limits, metrics });
    let listener = tokio::net::TcpListener::bind(config.listen).await.with_context(|| format!("binding {}", config.listen))?.tap_io(|tcp| {
        if let Err(error) = tcp.set_nodelay(true) {
            tracing::warn!(%error, "could not disable Nagle's algorithm");
        }
    });
    tracing::info!(listen = %config.listen, threads, "serving");
    axum::serve(listener, router(state, config.max_locations)).with_graceful_shutdown(shutdown_signal()).await?;
    Ok(())
}

#[cfg(test)]
mod tests;
