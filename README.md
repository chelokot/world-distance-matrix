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

**Compact** (`Accept: application/vnd.distance-matrix.compact.v2`) — the same exact values in about 3 % of the binary
size, for clients on the open internet. It carries distances in decimetres and times in milliseconds, the units the
routing core adds up exactly, and the decoder rounds them to metres and seconds exactly like the other formats. Rows and
columns are reordered along a Hilbert curve, and every point gets up to eight parents: the nearest earlier routable
points among the previous 1,024. A cell `(i, j)` is predicted as `v(i, p) + v(r, j) − v(r, p)` from a row parent `r` and
a column parent `p`; when the four routes meet in a common junction the prediction is exact to the decimetre and the
millisecond, which holds for ~89 % of cells with the nearest parents and ~97 % with the best of the 64 pairs. Each cell
is then coded as exact, unreachable, or the chosen pair plus a residual, by an adaptive binary range coder whose
contexts are the states of the cell's left and upper neighbours; residual mantissas travel as raw bits. The
1,000-point Eurasian demo matrix is 250 kB instead of 8 MB, 1,000 points around Hamburg 180 kB, and 20,000 Eurasian
points (400 million routes) 12 MB. All integers are unsigned 32-bit little-endian:

| offset | content                                                                                                  |
|--------|----------------------------------------------------------------------------------------------------------|
| 0      | magic `DMC2`                                                                                             |
| 4      | `rows`, `cols`, `shared` (1 when the columns are the rows)                                               |
| 16     | header section: row order and parents, then column order and parents unless shared                       |
| …      | ⌈rows / 32⌉ frame sections of 32 rows                                                                     |
| …      | server timings in microseconds: queue wait, until the first frame, until the last frame                  |

A section is its range-coded length, its raw length, the range-coded bytes and the raw bits. The bit-level model lives
in `crates/dm-wire/src/compact.rs`, where one function codes a cell for both the encoder and the decoder.
`dm_wire::compact::CompactDecoder` decodes while bytes arrive, keeps only the last 1,024 rows and hands every finished
row to a `RowSink`; the server's tests use it, the demo page runs it compiled to WebAssembly, and `bench/try_api.py`
loads it natively (`cargo build --release -p dm-web`). The timings at the end exist because rows are sent while later
ones are still being computed, so no header could carry them.

**Semantics**

* `[i][j]` is the route from `i` to `j`; matrices are asymmetric (one-way streets, one-way ferries/car trains).
* The route is the **fastest** route for a car or van; the reported distance is the length of that route. Ties in
  time are broken by shorter distance, so results are deterministic.
* Each coordinate is attached to the nearest road a car may drive through and stop on (within 5 km): motorways, their
  slip roads, expressways, tunnels, ferries and car trains carry routes but never take a point, so a house beside a
  motorway starts on its own street and a point at sea boards no ship. The straight line between the coordinate and
  that road belongs to every route that starts or ends there: its length is part of the distance and it is driven at
  15 km/h (a driveway, a yard, parking). Farm and forest tracks are not routed; a point more than 5 km from any other
  road is unroutable. Unroutable pairs — a point with no road within 5 km, a car-free
  island, a different continent — are `null` in JSON and `0xFFFFFFFF` in binary. A point to itself (or to an identical
  coordinate) is always `0`.
* Values are free-flow estimates from OpenStreetMap speed limits and road classes; they contain no live or historical
  traffic. Ferries take their tagged `duration`; a duration that would mean an impossible speed (faster than 80 km/h
  for a ship, 200 km/h for a car train) is a tagging mistake: a bare number is then read as hours instead of minutes
  when that is plausible, otherwise the ferry runs at 20 km/h. Roads that cross the 180th meridian are continuous.
  Seasonal closures (Alpine passes tagged `motor_vehicle:conditional=no @ (Nov-May)`) are closed in datasets built
  during the closure; the weekly rebuild keeps them current. Closures by time of day are not modelled.

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

| limit (default)                     | value                    |
|-------------------------------------|--------------------------|
| locations per request               | 25,000                   |
| cells per binary or compact request | 100,000,000 (10k × 10k); the public instance allows 400,000,000 (20k × 20k) |
| cells per JSON request              | 16,000,000 (4k × 4k)     |

### `GET /` — demo page

A single self-contained HTML file (`web/dist/matrix-demo.html`, built by `web/build.py` from `web/demo.html` and the
`dm-web` crate compiled to WebAssembly, embedded gzip-compressed). It spreads evenly spaced points over Eurasia that
are connected with Frankfurt by road, requests their matrix in any of the three formats, and shows where the time went:
connection, the way to the server, queue, snapping and search setup, computing rows, the way back, download and
decoding in the browser, which happens piece by piece while the answer is still arriving. It goes up to 20,000
points; above 25 million routes the page keeps statistics while decoding instead of the whole matrix and asks the server
for the pair you click. Two clicked points show their distance and time next to the straight line, with a link to
the same route in Google Maps. The page also works opened from disk; the API allows cross-origin calls and exposes
its timings to browsers.

### `GET /health`, `GET /metrics`

Liveness/readiness with the loaded dataset, and Prometheus metrics (`dm_requests_total`, `dm_compute_seconds`,
`dm_admission_wait_seconds`, `dm_cells_in_flight`, `dm_unsnapped_points_total`, `dm_dataset_info`, …).

## Try it

A public instance serves the whole planet at `http://3.65.232.220:8080`: one c5a.4xlarge in Frankfurt
(eu-central-1, availability zone ID `euc1-az2`). It is a demo and may be taken down.

Open the address in a browser for the demo page described above, or use a terminal:

```bash
curl -s http://3.65.232.220:8080/health
```

```bash
curl -s http://3.65.232.220:8080/matrix -H 'content-type: application/json' -d '{"coordinates":[{"lat":54.0,"lon":10.0},{"lat":54.1,"lon":10.1},{"lat":54.2,"lon":10.4}]}'
```

Time to last byte for 1,000 random points around a city, measured by a client that only needs Python's standard library:

```bash
python3 bench/try_api.py --points 1000 --center 53.55,10.0
```

`--format compact` uses the compact format, `--world` draws the points uniformly from the whole globe (most land in
the sea, more than 5 km from any road), and `--world --on-roads` keeps only points within 5 km of a road.
`python3 bench/sweep.py --region world` samples road-side points, requests their matrix and reports impossible cells
(shorter than the great circle, triangle violations, one-way reachability) and the most suspicious ones.

From outside AWS the result includes the internet round trip and the time your line needs for 8 MB. The 100 ms target
is for a client in the same region: run the script on any EC2 instance in eu-central-1, ideally in `euc1-az2`.

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
