use prometheus::{exponential_buckets, Encoder, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder};

pub struct Metrics {
    registry: Registry,
    pub requests: IntCounterVec,
    pub compute_seconds: HistogramVec,
    pub admission_wait_seconds: HistogramVec,
    pub cells: IntCounterVec,
    pub unsnapped_points: IntCounter,
    pub cells_in_flight: IntGauge,
    pub queued_requests: IntGauge,
    pub dataset: IntGaugeVec,
}

pub fn size_class(cells: usize) -> &'static str {
    match cells {
        0..=10_000 => "le_100x100",
        10_001..=250_000 => "le_500x500",
        250_001..=1_000_000 => "le_1000x1000",
        1_000_001..=4_000_000 => "le_2000x2000",
        4_000_001..=25_000_000 => "le_5000x5000",
        _ => "gt_5000x5000",
    }
}

impl Metrics {
    pub fn new() -> prometheus::Result<Self> {
        let registry = Registry::new_custom(Some("dm".into()), None)?;
        let requests = IntCounterVec::new(Opts::new("requests_total", "Matrix requests by outcome"), &["status", "format"])?;
        let compute_seconds = HistogramVec::new(
            HistogramOpts::new("compute_seconds", "Snapping, search and encoding time per request").buckets(exponential_buckets(0.0005, 2.0, 16)?),
            &["size"],
        )?;
        let admission_wait_seconds = HistogramVec::new(
            HistogramOpts::new("admission_wait_seconds", "Time spent waiting for compute capacity").buckets(exponential_buckets(0.0001, 4.0, 10)?),
            &["size"],
        )?;
        let cells = IntCounterVec::new(Opts::new("cells_total", "Matrix cells computed"), &["format"])?;
        let unsnapped_points = IntCounter::new("unsnapped_points_total", "Coordinates with no routable road within the snapping radius")?;
        let cells_in_flight = IntGauge::new("cells_in_flight", "Matrix cells admitted and not yet finished")?;
        let queued_requests = IntGauge::new("queued_requests", "Requests waiting for compute capacity")?;
        let dataset = IntGaugeVec::new(Opts::new("dataset_info", "Loaded routing dataset"), &["source", "profile", "built_at_unix"])?;
        registry.register(Box::new(requests.clone()))?;
        registry.register(Box::new(compute_seconds.clone()))?;
        registry.register(Box::new(admission_wait_seconds.clone()))?;
        registry.register(Box::new(cells.clone()))?;
        registry.register(Box::new(unsnapped_points.clone()))?;
        registry.register(Box::new(cells_in_flight.clone()))?;
        registry.register(Box::new(queued_requests.clone()))?;
        registry.register(Box::new(dataset.clone()))?;
        Ok(Self { registry, requests, compute_seconds, admission_wait_seconds, cells, unsnapped_points, cells_in_flight, queued_requests, dataset })
    }

    pub fn render(&self) -> Vec<u8> {
        let mut out = Vec::new();
        TextEncoder::new().encode(&self.registry.gather(), &mut out).expect("text encoding into a Vec cannot fail");
        out
    }
}
