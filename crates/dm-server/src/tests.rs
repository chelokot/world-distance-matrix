use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use dm_build::{build, BuildConfig};
use dm_core::compact::{decode_compact, COMPACT_CONTENT_TYPE};
use dm_core::geo::Coord;
use dm_core::network::Network;
use dm_core::snap::{snap, SnapConfig};
use dm_core::store::Residency;
use dm_core::wire::{decode_binary, BINARY_CONTENT_TYPE};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::api::Limits;
use crate::engine::{AdmissionConfig, Engine};
use crate::metrics::Metrics;
use crate::{router, AppState};

fn dataset() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let output = std::env::temp_dir().join(format!("dm-server-test-{}", std::process::id()));
        build(&BuildConfig {
            input: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/kiel.osm.pbf"),
            output: output.clone(),
            witness_settle_limit: 500,
            major_component_min_nodes: 100,
            simplify_tolerance_m: 5.0,
        })
        .expect("building the Kiel fixture");
        output
    })
}

fn app_with(capacity_cells: usize, max_queued: usize) -> Router {
    let network = Network::open(dataset(), Residency::OnDemand).expect("opening fixture");
    let metrics = Arc::new(Metrics::new().expect("metrics"));
    let admission = AdmissionConfig { capacity_cells, max_queued, queue_timeout: Duration::from_millis(500) };
    let engine = Arc::new(Engine::new(network, 4, SnapConfig::default(), 4096, admission, Arc::clone(&metrics)).expect("engine"));
    let limits = Limits { max_locations: 1000, max_cells: 250_000, max_json_cells: 10_000 };
    router(Arc::new(AppState { engine, limits, metrics }), 1000)
}

fn app() -> Router {
    app_with(1_000_000, 16)
}

async fn call(app: Router, method: &str, uri: &str, body: String, accept: Option<&str>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(accept) = accept {
        request = request.header(header::ACCEPT, accept);
    }
    let response = app.oneshot(request.body(Body::from(body)).expect("request")).await.expect("response");
    let (parts, body) = response.into_parts();
    (parts.status, parts.headers, body.collect().await.expect("body").to_bytes().to_vec())
}

fn kiel_points(count: usize) -> Vec<(f64, f64)> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 1_000_000) as f64 / 1_000_000.0
    };
    (0..count).map(|_| (54.305 + next() * 0.04, 10.105 + next() * 0.06)).collect()
}

fn body_for(points: &[(f64, f64)], extra: Value) -> String {
    let mut body = json!({ "coordinates": points.iter().map(|&(lat, lon)| json!({ "lat": lat, "lon": lon })).collect::<Vec<_>>() });
    body.as_object_mut().expect("object").extend(extra.as_object().expect("object").clone());
    body.to_string()
}

type JsonMatrix = Vec<Vec<Option<u64>>>;

fn json_matrix(bytes: &[u8]) -> (JsonMatrix, JsonMatrix) {
    let value: Value = serde_json::from_slice(bytes).expect("json body");
    let matrix = |key: &str| -> JsonMatrix {
        value[key].as_array().expect("rows").iter().map(|row| row.as_array().expect("row").iter().map(Value::as_u64).collect()).collect()
    };
    (matrix("distances"), matrix("times"))
}

#[tokio::test]
async fn health_reports_the_dataset() {
    let (status, _, body) = call(app(), "GET", "/health", String::new(), None).await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["status"], "ok");
    assert_eq!(value["dataset"]["profile"], "car-v2");
}

#[tokio::test]
async fn json_matrix_has_the_specified_shape() {
    let points = [(54.3233, 10.1394), (54.3300, 10.1450), (54.3150, 10.1300)];
    let (status, headers, body) = call(app(), "POST", "/matrix", body_for(&points, json!({})), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    let timing = headers["server-timing"].to_str().unwrap();
    let durations: Vec<(&str, f64)> =
        timing.split(", ").map(|entry| entry.split_once(";dur=").map(|(name, ms)| (name, ms.parse().unwrap())).expect("name;dur=ms")).collect();
    assert!(matches!(durations[..], [("queue", queue), ("compute", compute)] if queue >= 0.0 && compute > 0.0), "{timing}");
    let (distances, times) = json_matrix(&body);
    for i in 0..3 {
        assert_eq!(distances[i][i], Some(0));
        assert_eq!(times[i][i], Some(0));
        for j in (0..3).filter(|&j| j != i) {
            let (d, t) = (distances[i][j].unwrap(), times[i][j].unwrap());
            assert!((100..10_000).contains(&d), "{i}->{j}: {d} m");
            assert!((10..1_800).contains(&t), "{i}->{j}: {t} s");
        }
    }
}

#[tokio::test]
async fn binary_and_json_agree_and_reflect_one_way_streets() {
    let points = kiel_points(60);
    let (status, headers, binary) = call(app(), "POST", "/matrix", body_for(&points, json!({})), Some(BINARY_CONTENT_TYPE)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], BINARY_CONTENT_TYPE);
    assert!(headers["server-timing"].to_str().unwrap().starts_with("queue;dur="));
    assert_eq!(headers[header::CONTENT_LENGTH].to_str().unwrap().parse::<usize>().unwrap(), binary.len());
    let decoded = decode_binary(&binary).unwrap();
    assert_eq!((decoded.rows, decoded.cols), (60, 60));
    let (status, _, json_body) = call(app(), "POST", "/matrix", body_for(&points, json!({})), Some("application/json")).await;
    assert_eq!(status, StatusCode::OK);
    let (distances, times) = json_matrix(&json_body);
    let mut asymmetric = 0;
    for i in 0..60 {
        for j in 0..60 {
            assert_eq!(decoded.distance(i, j).map(u64::from), distances[i][j]);
            assert_eq!(decoded.duration(i, j).map(u64::from), times[i][j]);
            asymmetric += usize::from(decoded.duration(i, j) != decoded.duration(j, i));
        }
    }
    assert!(asymmetric > 0, "central Kiel has one-way streets, so some pairs must differ by direction");
}

#[tokio::test]
async fn json_is_compressed_when_asked_and_binary_never() {
    let body = body_for(&kiel_points(60), json!({}));
    let (_, _, plain) = call(app(), "POST", "/matrix", body.clone(), None).await;
    let request = |accept: &str| {
        Request::builder()
            .method("POST")
            .uri("/matrix")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, accept)
            .header(header::ACCEPT_ENCODING, "gzip")
            .body(Body::from(body.clone()))
            .expect("request")
    };
    let response = app().oneshot(request("application/json")).await.unwrap();
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    let compressed = response.into_body().collect().await.unwrap().to_bytes();
    let mut unpacked = Vec::new();
    flate2::read::GzDecoder::new(&compressed[..]).read_to_end(&mut unpacked).unwrap();
    assert_eq!(unpacked, plain);
    let response = app().oneshot(request(BINARY_CONTENT_TYPE)).await.unwrap();
    assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
}

#[tokio::test]
async fn compact_bodies_decode_to_the_binary_matrix() {
    let mut points = kiel_points(300);
    points.extend([(54.6, 10.9), points[5]]);
    for extra in [json!({}), json!({ "sources": (0..70).map(|i| i * 4).collect::<Vec<_>>() })] {
        let body = body_for(&points, extra);
        let (_, _, binary) = call(app(), "POST", "/matrix", body.clone(), Some(BINARY_CONTENT_TYPE)).await;
        let (status, headers, compact) = call(app(), "POST", "/matrix", body, Some(COMPACT_CONTENT_TYPE)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], COMPACT_CONTENT_TYPE);
        assert!(compact.len() * 3 < binary.len(), "{} compact bytes vs {} binary", compact.len(), binary.len());
        assert_eq!(decode_compact(&compact).unwrap(), decode_binary(&binary).unwrap());
    }
}

#[tokio::test]
async fn rectangular_requests_match_the_square_matrix() {
    let points = kiel_points(12);
    let (_, _, square) = call(app(), "POST", "/matrix", body_for(&points, json!({})), None).await;
    let (square_distances, square_times) = json_matrix(&square);
    let extra = json!({ "sources": [2, 7], "destinations": [0, 5, 11, 2] });
    let (status, _, body) = call(app(), "POST", "/matrix", body_for(&points, extra), None).await;
    assert_eq!(status, StatusCode::OK);
    let (distances, times) = json_matrix(&body);
    for (r, &source) in [2usize, 7].iter().enumerate() {
        for (c, &target) in [0usize, 5, 11, 2].iter().enumerate() {
            assert_eq!(distances[r][c], square_distances[source][target]);
            assert_eq!(times[r][c], square_times[source][target]);
        }
    }
}

#[tokio::test]
async fn duplicates_are_free_and_unroutable_points_are_null() {
    let points = [(54.3233, 10.1394), (54.3233, 10.1394), (54.6, 10.9), (54.3300, 10.1450)];
    let (status, _, body) = call(app(), "POST", "/matrix", body_for(&points, json!({})), None).await;
    assert_eq!(status, StatusCode::OK);
    let (distances, times) = json_matrix(&body);
    assert_eq!((distances[0][1], times[1][0]), (Some(0), Some(0)));
    assert_eq!((distances[2][2], times[2][2]), (Some(0), Some(0)));
    for other in [0, 1, 3] {
        assert_eq!(distances[2][other], None);
        assert_eq!(times[other][2], None);
    }
    assert!(distances[0][3].is_some());
}

#[tokio::test]
async fn off_road_points_pay_for_the_way_to_the_road() {
    let network = Network::open(dataset(), Residency::OnDemand).expect("opening fixture");
    let (point, snapped) = kiel_points(400)
        .into_iter()
        .find_map(|(lat, lon)| {
            snap(&network, Coord::from_degrees(lat, lon), &SnapConfig::default()).filter(|s| (60.0..400.0).contains(&s.distance_m)).map(|s| ((lat, lon), s))
        })
        .expect("an off-road point in the fixture");
    let neighbour = (point.0 + 1e-6, point.1);
    let (status, _, body) = call(app(), "POST", "/matrix", body_for(&[point, neighbour], json!({})), None).await;
    assert_eq!(status, StatusCode::OK);
    let (distances, times) = json_matrix(&body);
    let shorter = |m: &JsonMatrix| m[0][1].unwrap().min(m[1][0].unwrap()) as f64;
    let to_road_and_back_m = 2.0 * snapped.distance_m;
    assert!((shorter(&distances) - to_road_and_back_m).abs() <= 2.0, "{distances:?} vs {to_road_and_back_m} m");
    assert!((shorter(&times) - to_road_and_back_m * 3.6 / 15.0).abs() <= 2.0, "{times:?} vs {to_road_and_back_m} m at 15 km/h");
}

#[tokio::test]
async fn invalid_requests_are_rejected_with_explanations() {
    let cases: Vec<(String, Option<&str>, StatusCode)> = vec![
        ("not json".into(), None, StatusCode::BAD_REQUEST),
        (json!({ "coordinates": [] }).to_string(), None, StatusCode::BAD_REQUEST),
        (json!({ "coordinates": [{ "lat": 100.0, "lon": 10.0 }] }).to_string(), None, StatusCode::BAD_REQUEST),
        (json!({ "coordinates": [{ "lat": 54.0, "lng": 10.0 }] }).to_string(), None, StatusCode::BAD_REQUEST),
        (json!({ "coordinates": [{ "lat": 54.0, "lon": 10.0 }], "sources": [3] }).to_string(), None, StatusCode::BAD_REQUEST),
        (body_for(&kiel_points(2), json!({})), Some("text/html"), StatusCode::NOT_ACCEPTABLE),
        (body_for(&kiel_points(101), json!({})), None, StatusCode::PAYLOAD_TOO_LARGE),
        (body_for(&kiel_points(501), json!({})), Some(BINARY_CONTENT_TYPE), StatusCode::PAYLOAD_TOO_LARGE),
        (body_for(&kiel_points(1001), json!({ "sources": [0], "destinations": [1] })), Some(BINARY_CONTENT_TYPE), StatusCode::PAYLOAD_TOO_LARGE),
    ];
    for (body, accept, expected) in cases {
        let (status, headers, response) = call(app(), "POST", "/matrix", body.clone(), accept).await;
        assert_eq!(status, expected, "{}", &body[..body.len().min(80)]);
        if status != StatusCode::PAYLOAD_TOO_LARGE || headers.get(header::CONTENT_TYPE).is_some_and(|v| v == "application/json") {
            let value: Value = serde_json::from_slice(&response).unwrap();
            assert!(value["error"].as_str().is_some_and(|e| !e.is_empty()));
        }
    }
}

#[tokio::test]
async fn overload_is_reported_as_retryable() {
    let (status, headers, _) = call(app_with(1_000_000, 0), "POST", "/matrix", body_for(&kiel_points(3), json!({})), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers[header::RETRY_AFTER], "1");
}

#[tokio::test]
async fn metrics_are_exposed() {
    let app = app();
    call(app.clone(), "POST", "/matrix", body_for(&kiel_points(3), json!({})), None).await;
    let (status, _, body) = call(app, "GET", "/metrics", String::new(), None).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("dm_requests_total{format=\"json\",status=\"200\"} 1"));
    assert!(text.contains("dm_dataset_info"));
}

#[tokio::test]
async fn streams_large_binary_matrices_over_tcp() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app()).await.unwrap() });
    let points = kiel_points(400);
    let response = reqwest::Client::new()
        .post(format!("http://{address}/matrix"))
        .header(header::ACCEPT, BINARY_CONTENT_TYPE)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body_for(&points, json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.bytes().await.unwrap();
    let streamed = decode_binary(&body).unwrap();
    let (_, _, again) = call(app(), "POST", "/matrix", body_for(&points, json!({})), Some(BINARY_CONTENT_TYPE)).await;
    assert_eq!(streamed, decode_binary(&again).unwrap());
    assert!((0..400).all(|i| streamed.distance(i, i) == Some(0)));
}
