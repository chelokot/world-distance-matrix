use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderValue};
use axum::response::Response;
use bytes::Bytes;
use dm_core::compact::{compact_header, encode_frame, spatial_order, Piece, Transport, TransportEncoder};
use dm_core::matrix::{Endpoint, MatrixJob};
use dm_core::network::Network;
use dm_core::snap::{snap, SnapConfig};
use dm_core::wire::encode_json;
use dm_wire::compact::{ServerTimes, FRAME_ROWS};
use dm_wire::{binary, compact};
use rayon::prelude::*;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

use crate::api::{ApiError, Format, MatrixSpec};
use crate::metrics::{size_class, Metrics};

const _: () = assert!(cfg!(target_endian = "little"), "the binary wire format is emitted directly from native little-endian memory");

pub struct AdmissionConfig {
    pub capacity_cells: usize,
    pub max_queued: usize,
    pub queue_timeout: Duration,
}

const LIGHT_LANE_MAX_CELLS: usize = 250_000;
const HEAVY_LANE_MAX_CELLS: usize = 4_000_000;

struct Lane {
    pool: rayon::ThreadPool,
    turn: Arc<Semaphore>,
}

impl Lane {
    fn new(name: &'static str, threads: usize) -> anyhow::Result<Self> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(move |index| format!("{name}-{index}"))
            .panic_handler(move |_| tracing::error!(lane = name, "a matrix worker panicked"))
            .build()?;
        Ok(Self { pool, turn: Arc::new(Semaphore::new(1)) })
    }
}

pub struct Engine {
    network: Network,
    lanes: [Lane; 3],
    snap: SnapConfig,
    block_bytes: usize,
    semaphore: Arc<Semaphore>,
    admission: AdmissionConfig,
    queued: AtomicUsize,
    metrics: Arc<Metrics>,
}

struct Reservation {
    _permit: OwnedSemaphorePermit,
    cells: i64,
    metrics: Arc<Metrics>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.metrics.cells_in_flight.sub(self.cells);
    }
}

struct Admitted {
    reservation: Reservation,
    turn: OwnedSemaphorePermit,
    waited: Duration,
}

fn server_timing(entries: &[(&str, Duration)]) -> HeaderValue {
    let value = entries.iter().map(|(name, duration)| format!("{name};dur={:.1}", duration.as_secs_f64() * 1e3)).collect::<Vec<_>>().join(", ");
    HeaderValue::from_str(&value).expect("server timing is plain ASCII")
}

struct WordBlock(Vec<u32>);

impl AsRef<[u8]> for WordBlock {
    fn as_ref(&self) -> &[u8] {
        bytemuck::cast_slice(&self.0)
    }
}

type BodySender = mpsc::UnboundedSender<Result<Bytes, std::io::Error>>;

struct Sequencer<'a> {
    next: usize,
    waiting: BTreeMap<usize, Piece>,
    wire: &'a mut TransportEncoder,
    body_tx: &'a BodySender,
    delivering: bool,
}

impl Sequencer<'_> {
    fn deliver(&mut self, index: usize, piece: Piece) {
        self.waiting.insert(index, piece);
        while let Some(piece) = self.waiting.remove(&self.next) {
            self.next += 1;
            self.delivering &= self.body_tx.send(Ok(Bytes::from(self.wire.push(piece)))).is_ok();
        }
    }
}

fn stream_frames(job: &MatrixJob, transport: Transport, wire: &mut TransportEncoder, body_tx: &BodySender) -> bool {
    let (rows, cols) = (job.source_count(), job.target_count());
    let sequencer = Mutex::new(Sequencer { next: 0, waiting: BTreeMap::new(), wire, body_tx, delivering: true });
    rayon::scope_fifo(|scope| {
        for (index, start) in (0..rows).step_by(FRAME_ROWS).enumerate() {
            let sequencer = &sequencer;
            scope.spawn_fifo(move |_| {
                if !sequencer.lock().expect("sequencer lock").delivering {
                    return;
                }
                let (first, end) = (start.saturating_sub(1), (start + FRAME_ROWS).min(rows));
                let mut block = vec![0u32; (end - first) * 2 * cols];
                job.compute_rows_on_this_thread(first..end, &mut block);
                let (above, own) = block.split_at((start - first) * 2 * cols);
                let piece = transport.encode(encode_frame(cols, (start > 0).then_some(above), own));
                sequencer.lock().expect("sequencer lock").deliver(index, piece);
            });
        }
    });
    sequencer.into_inner().expect("sequencer lock").delivering
}

impl Engine {
    pub fn new(
        network: Network,
        threads: usize,
        snap: SnapConfig,
        block_bytes: usize,
        admission: AdmissionConfig,
        metrics: Arc<Metrics>,
    ) -> anyhow::Result<Self> {
        let lanes = [Lane::new("light", threads)?, Lane::new("heavy", threads)?, Lane::new("bulk", threads)?];
        let manifest = &network.manifest;
        metrics.dataset.with_label_values(&[&manifest.source, &manifest.profile, &manifest.built_at_unix.to_string()]).set(1);
        let semaphore = Arc::new(Semaphore::new(admission.capacity_cells));
        Ok(Self { network, lanes, snap, block_bytes, semaphore, admission, queued: AtomicUsize::new(0), metrics })
    }

    pub fn network(&self) -> &Network {
        &self.network
    }

    fn lane(&self, cells: usize) -> &Lane {
        let index = if cells <= LIGHT_LANE_MAX_CELLS {
            0
        } else if cells <= HEAVY_LANE_MAX_CELLS {
            1
        } else {
            2
        };
        &self.lanes[index]
    }

    async fn admit(&self, cells: usize) -> Result<Admitted, ApiError> {
        let permits = cells.clamp(1, self.admission.capacity_cells) as u32;
        if self.queued.fetch_add(1, Ordering::AcqRel) >= self.admission.max_queued {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            return Err(ApiError::Overloaded);
        }
        self.metrics.queued_requests.inc();
        let started = Instant::now();
        let deadline = tokio::time::Instant::now() + self.admission.queue_timeout;
        let turn = Arc::clone(&self.lane(cells).turn);
        let acquired = async {
            let capacity = tokio::time::timeout_at(deadline, Arc::clone(&self.semaphore).acquire_many_owned(permits)).await;
            let capacity = capacity.map_err(|_| ApiError::Overloaded)?.map_err(|_| ApiError::Internal)?;
            let turn = tokio::time::timeout_at(deadline, turn.acquire_owned()).await;
            Ok::<_, ApiError>((capacity, turn.map_err(|_| ApiError::Overloaded)?.map_err(|_| ApiError::Internal)?))
        }
        .await;
        self.queued.fetch_sub(1, Ordering::AcqRel);
        self.metrics.queued_requests.dec();
        let waited = started.elapsed();
        self.metrics.admission_wait_seconds.with_label_values(&[size_class(cells)]).observe(waited.as_secs_f64());
        let (capacity, turn) = acquired?;
        self.metrics.cells_in_flight.add(cells as i64);
        let reservation = Reservation { _permit: capacity, cells: cells as i64, metrics: Arc::clone(&self.metrics) };
        Ok(Admitted { reservation, turn, waited })
    }

    fn prepare(&self, spec: &MatrixSpec) -> MatrixJob<'_> {
        let endpoints: Vec<Endpoint> = spec.coords.par_iter().map(|&coord| Endpoint { coord, snap: snap(&self.network, coord, &self.snap) }).collect();
        self.metrics.unsnapped_points.inc_by(endpoints.iter().filter(|e| e.snap.is_none()).count() as u64);
        let pick = |indices: &[usize]| indices.iter().map(|&i| endpoints[i]).collect();
        MatrixJob::prepare(&self.network, pick(&spec.sources), pick(&spec.destinations))
    }

    pub async fn matrix(self: &Arc<Self>, spec: MatrixSpec) -> Result<Response, ApiError> {
        let admitted = self.admit(spec.cells()).await?;
        match spec.format {
            Format::Binary | Format::Compact => self.stream(spec, admitted).await,
            Format::Json => self.json(spec, admitted).await,
        }
    }

    fn finish(&self, spec: &MatrixSpec, started: Instant, outcome: std::thread::Result<()>) {
        match outcome {
            Ok(()) => {
                let compute = started.elapsed();
                tracing::info!(
                    rows = spec.sources.len(),
                    cols = spec.destinations.len(),
                    format = spec.format.label(),
                    compute_ms = compute.as_secs_f64() * 1e3,
                    "matrix computed"
                );
                self.metrics.compute_seconds.with_label_values(&[size_class(spec.cells())]).observe(compute.as_secs_f64());
                self.metrics.cells.with_label_values(&[spec.format.label()]).inc_by(spec.cells() as u64);
            }
            Err(_) => tracing::error!(rows = spec.sources.len(), cols = spec.destinations.len(), "matrix computation panicked"),
        }
    }

    async fn stream(self: &Arc<Self>, mut spec: MatrixSpec, admitted: Admitted) -> Result<Response, ApiError> {
        let (rows, cols) = (spec.sources.len(), spec.destinations.len());
        let compact = spec.format == Format::Compact;
        let transport = if compact { spec.transport } else { Transport::Identity };
        let mut wire = TransportEncoder::new(transport);
        let head = if compact {
            let order = |indices: &[usize]| spatial_order(&indices.iter().map(|&i| spec.coords[i]).collect::<Vec<_>>());
            let (row_order, col_order) = (order(&spec.sources), order(&spec.destinations));
            spec.sources = row_order.iter().map(|&k| spec.sources[k as usize]).collect();
            spec.destinations = col_order.iter().map(|&k| spec.destinations[k as usize]).collect();
            [wire.start(), wire.push(transport.encode(compact_header(&row_order, &col_order)))].concat()
        } else {
            binary::header(rows as u32, cols as u32).to_vec()
        };
        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        let (body_tx, body_rx) = mpsc::unbounded_channel::<Result<Bytes, std::io::Error>>();
        let engine = Arc::clone(self);
        let Admitted { reservation, turn, waited } = admitted;
        self.lane(spec.cells()).pool.spawn(move || {
            let started = Instant::now();
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let job = engine.prepare(&spec);
                if ready_tx.send(()).is_err() || body_tx.send(Ok(Bytes::from(head))).is_err() {
                    return;
                }
                let prepared = started.elapsed();
                if !compact {
                    let rows_per_block = (engine.block_bytes / (8 * cols)).max(1);
                    for start in (0..rows).step_by(rows_per_block) {
                        let end = (start + rows_per_block).min(rows);
                        let mut block = vec![0u32; (end - start) * 2 * cols];
                        job.compute_rows(start..end, &mut block);
                        if body_tx.send(Ok(Bytes::from_owner(WordBlock(block)))).is_err() {
                            return;
                        }
                    }
                    return;
                }
                if stream_frames(&job, transport, &mut wire, &body_tx) {
                    let micros = |duration: Duration| duration.as_micros() as u32;
                    let times = ServerTimes { queue_us: micros(waited), prepare_us: micros(prepared), compute_us: micros(started.elapsed()) };
                    let _ = body_tx.send(Ok(Bytes::from([wire.push(transport.encode(times.to_bytes())), wire.finish()].concat())));
                }
            }));
            if outcome.is_err() {
                let _ = body_tx.send(Err(std::io::Error::other("matrix computation failed")));
            }
            engine.finish(&spec, started, outcome);
            drop(turn);
        });
        ready_rx.await.map_err(|_| ApiError::Internal)?;
        let body = UnboundedReceiverStream::new(body_rx).map(move |chunk| {
            let _held_until_the_body_is_dropped = &reservation;
            chunk
        });
        let mut response = Response::new(Body::from_stream(body));
        let headers = response.headers_mut();
        if compact {
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(compact::CONTENT_TYPE));
            headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
            if let Some(encoding) = transport.content_encoding() {
                headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
            }
        } else {
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(binary::CONTENT_TYPE));
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(binary::len(rows, cols)));
        }
        headers.insert("server-timing", server_timing(&[("queue", waited)]));
        Ok(response)
    }

    async fn json(self: &Arc<Self>, spec: MatrixSpec, admitted: Admitted) -> Result<Response, ApiError> {
        let (tx, rx) = oneshot::channel::<(Bytes, Duration)>();
        let engine = Arc::clone(self);
        let waited = admitted.waited;
        self.lane(spec.cells()).pool.spawn(move || {
            let started = Instant::now();
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let (rows, cols) = (spec.sources.len(), spec.destinations.len());
                let job = engine.prepare(&spec);
                let mut values = vec![0u32; rows * 2 * cols];
                job.compute_rows(0..rows, &mut values);
                let _ = tx.send((Bytes::from(encode_json(&values, rows, cols)), started.elapsed()));
            }));
            engine.finish(&spec, started, outcome);
            drop(admitted);
        });
        let (body, computed) = rx.await.map_err(|_| ApiError::Internal)?;
        let mut response = Response::new(Body::from(body));
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("server-timing", server_timing(&[("queue", waited), ("compute", computed)]));
        Ok(response)
    }
}
