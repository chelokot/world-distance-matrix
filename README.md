# dm — road distance-matrix service

A single-endpoint web service that returns road travel **distances (m)** and **times (s)** between every pair of a set of
coordinates, computed on the OpenStreetMap road network of the whole world. It is built to feed a vehicle-routing
optimiser: 1,000 × 1,000 matrices are served in tens of milliseconds, time-to-last-byte, to a client in the same cloud
region.

Measured on one c5a.4xlarge with the whole planet loaded, from a client in the same AWS availability zone: 1,000 × 1,000
in 31–36 ms median and under 40 ms p99 time-to-last-byte; 100 × 100 in ~4 ms; booking-style 1 × 1,000 rows in ~11 ms.
The design, the trade-offs and the measurements behind them are in [docs/REPORT.md](docs/REPORT.md).

## API

### `POST /matrix`

```json
{
  "coordinates": [
    { "lat": 54.0, "lon": 10.0 },
    { "lat": 54.1, "lon": 10.1 },
    { "lat": 54.2, "lon": 10.4 }
  ]
}
```

Optional fields, for incremental updates (e.g. one new job against an existing plan):

| field          | meaning                                                                                  |
|----------------|------------------------------------------------------------------------------------------|
| `sources`      | indices into `coordinates` used as matrix rows (default: all)                           |
| `destinations` | indices into `coordinates` used as matrix columns (default: all)                        |

The response format is chosen with the `Accept` header.

**JSON** (`Accept: application/json`, or no `Accept` header):

```json
{ "distances": [[0, 18070, 48928], [18062, 0, 36030], [48894, 36030, 0]],
  "times":     [[0, 1213, 3080],   [1200, 0, 2665],   [3070, 2665, 0]] }
```

**Binary** (`Accept: application/vnd.distance-matrix.v1`) — the fast path, recommended for anything above a few
hundred locations. All integers are unsigned 32-bit little-endian:

| offset | content                                                                                                    |
|--------|------------------------------------------------------------------------------------------------------------|
| 0      | magic `DMX1`                                                                                               |
| 4      | the value used for "no route": `0xFFFFFFFF`                                                                |
| 8      | `rows`                                                                                                     |
| 12     | `cols`                                                                                                     |
| 16     | for each row `i`: `cols` distances in metres, then `cols` times in seconds                                  |

So `distance(i, j)` is the word at `16 + 4 * (2 * cols * i + j)` and `time(i, j)` the word at
`16 + 4 * (2 * cols * i + cols + j)`. In numpy: `m = np.frombuffer(body, "<u4", offset=16).reshape(rows, 2, cols)`,
then `m[:, 0]` are distances and `m[:, 1]` times (zero-copy views). The body is streamed while rows are still being
computed, with an exact `Content-Length`; a truncated body means the request failed.

**Semantics**

* `[i][j]` is the route from `i` to `j`; matrices are asymmetric (one-way streets, one-way ferries/car trains).
* The route is the **fastest** route for a car or van; the reported distance is the length of that route. Ties in
  time are broken by shorter distance, so results are deterministic.
* Each coordinate is attached to the nearest road a car may drive through (within 5 km). The straight line between the
  coordinate and that road belongs to every route that starts or ends there: its length is part of the distance and it
  is driven at 15 km/h (a driveway, a yard, parking). Unroutable pairs — a point with no road within 5 km, a car-free
  island, a different continent — are `null` in JSON and `0xFFFFFFFF` in binary. A point to itself (or to an identical
  coordinate) is always `0`.
* Values are free-flow estimates from OpenStreetMap speed limits and road classes; they contain no live or historical
  traffic.

Every matrix response carries a `Server-Timing` header with the time the request waited for capacity and, for JSON,
the server's compute time including encoding: `server-timing: queue;dur=0.1, compute;dur=25.4` (milliseconds). Binary
responses carry only `queue`, because their rows are sent while later rows are still being computed. To see it next to
the client's own time: `curl -s -o out.json -w '%header{server-timing} | total %{time_total}s\n' ...`.

JSON responses are compressed (zstd or gzip, fastest level) when the request allows it with `Accept-Encoding`, as
Postman, Bruno, HTTPie and browsers do by default and curl does with `--compressed`: 1,000² shrinks from 10.5 MB to
4.4–5.2 MB. Binary responses are never compressed; they are the fast path inside a region, where 8 MB moves in ~13 ms
and compressing would cost more than it saves.

**Errors** are JSON `{"error": "..."}`: `400` malformed request, `406` unsupported `Accept`, `413` too many
locations/cells (limits below), `503` at capacity (with `Retry-After`; retry with back-off).

| limit (default)          | value                    |
|--------------------------|--------------------------|
| locations per request    | 25,000                   |
| cells per binary request | 100,000,000 (10k × 10k)  |
| cells per JSON request   | 16,000,000 (4k × 4k)     |

### `GET /health`, `GET /metrics`

Liveness/readiness with the loaded dataset, and Prometheus metrics (`dm_requests_total`, `dm_compute_seconds`,
`dm_admission_wait_seconds`, `dm_cells_in_flight`, `dm_unsnapped_points_total`, `dm_dataset_info`, …).

## Try it

Public instances serve the whole planet, each on one c5a.4xlarge: `http://3.65.232.220:8080` in Frankfurt
(eu-central-1, availability zone ID `euc1-az2`) and `http://100.57.61.188:8080` in northern Virginia (us-east-1,
`use1-az5`). They are a demo and may be taken down.

```bash
curl -s http://100.57.61.188:8080/health
```

```bash
curl -s http://100.57.61.188:8080/matrix -H 'content-type: application/json' -d '{"coordinates":[{"lat":54.0,"lon":10.0},{"lat":54.1,"lon":10.1},{"lat":54.2,"lon":10.4}]}'
```

Time to last byte for 1,000 random points around a city, measured by a client that only needs Python's standard library:

```bash
python3 bench/try_api.py --points 1000 --center 53.55,10.0
```

From outside AWS the result includes the internet round trip and the time your line needs for 8 MB. The 100 ms target
is for a client in the same region: run the script on any EC2 instance in us-east-1, ideally in `use1-az5`.

## Running it

```bash
cargo build --release
./target/release/dm-build --input planet.osm.pbf --output /data/planet-2026-09-21
./target/release/dm-server --data /data/planet-2026-09-21
```

Configuration is by flag or environment variable (`dm-server --help`): `DM_DATA`, `DM_LISTEN`, `DM_THREADS`,
`DM_MAX_LOCATIONS`, `DM_MAX_CELLS`, `DM_MAX_JSON_CELLS`, `DM_CAPACITY_CELLS`, `DM_MAX_QUEUED`, `DM_QUEUE_TIMEOUT_MS`,
`DM_SNAP_MAX_DISTANCE_M`, `DM_LOCK_MEMORY`, `DM_LOG_JSON`.

Production deployment (systemd unit, kernel tuning, versioned datasets with an atomic `current` symlink) is in
[infra/deploy](infra/deploy); on a host, `infra/deploy/upgrade.sh <dataset>` installs the current release binary and
that dataset from S3 and restarts the service, keeping one previous dataset for rollback.

## Repository layout

| path               | contents                                                                                  |
|--------------------|-------------------------------------------------------------------------------------------|
| `crates/dm-core`   | dataset format, snapping, contraction-hierarchy search, many-to-many engine, wire format   |
| `crates/dm-build`  | OSM import, vehicle profile, topology, components, contraction, dataset writer           |
| `crates/dm-server` | HTTP service, admission control, metrics, end-to-end tests                                |
| `crates/dm-bench`  | point sampler, correctness verifier, OSRM comparison, engine profiler, HTTP load generator |
| `bench`            | VRP sensitivity experiment (PyVRP)                                                        |
| `infra`            | provisioning and deployment scripts                                                       |
| `testdata`         | small Kiel road extract used by the end-to-end tests (© OpenStreetMap contributors, ODbL) |

## Tests

```bash
cargo test --release --workspace
```

Unit and property tests (contraction hierarchy against Dijkstra on random graphs, spatial index against brute force,
SIMD kernel against scalar code, profile tag handling) and end-to-end tests that build a dataset from real OSM data and
exercise the HTTP API over TCP. `dm-bench verify` checks a full dataset against an independent Dijkstra.

## License

The code is MIT-licensed (see [LICENSE](LICENSE)). Road data and the Kiel test extract are © OpenStreetMap
contributors under the Open Database License.
