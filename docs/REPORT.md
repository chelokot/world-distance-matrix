# Road distance matrices for a field-service optimiser — engineering report

## 0. Summary

A single-endpoint service that returns road distances and travel times between every pair of up to 10,000 locations
anywhere in the world, built for the scheduling optimiser of Solvares Field Service (VISITOUR). Running on one
c5a.4xlarge with the whole OpenStreetMap planet (235 M junctions, 1.9 M turn restrictions) resident in RAM:

* **1,000 × 1,000 in 31–36 ms median, < 40 ms p99, time to last byte** at a client in the same AWS availability zone,
  for realistic points from Hamburg to Europe-wide to New York, Tokyo or Sydney (OSRM's table service needs 1.1 s for
  the same size on a single-city extract).
* **Exact** fastest routes on the road model: 100 % agreement (to 1 m rounding) with an independent Dijkstra on the
  world graph; one-ways, directional limits and turn restrictions reflected; deterministic.
* **Stable under load**: FIFO lanes keep latency proportional to queue length (no multi-second tails), batch-size
  requests cannot block interactive ones, overload is a fast `503`, the dataset is locked in RAM and validated at load.
* **What was relaxed, and why it is safe:** the 100 ms applies to an in-region client (the optimiser runs on AWS) using
  the binary format (JSON parsing alone costs 188 ms in Python); turn *delays* are not modelled (≈ 1.4 % optimistic in
  cities vs OSRM's turn model), which a VRP experiment shows leaves plan efficiency unchanged and costs ≈ 8 s of
  schedule slack per technician-day; free-flow speeds (the customer applies its own traffic profiles).
* **Checked against the task's own example** (§5): where our times differed from OSRM's, the routes were identical for
  17–45 km and agreed within 0.4–3 %; the gap sat in the last few hundred metres, where the example's coordinates (points
  in fields, 250–600 m from a road) attach to different roads. That led to release `car-v2`: the way from a point to
  its road is now charged instead of free, and routes no longer cut through car parks.
* **Public demo:** `http://100.57.61.188:8080` (see the README for a one-command latency test).

## 1. Who the customer is and what they actually need

Stäfn (Steffen Trog, "KnorpelSenf") lists **Solvares Field Service** in Kiel as his employer. Solvares Field Service is
the former FLS (FAST LEAN SMART, Heikendorf near Kiel, founded 1992), merged with mobileX into the Solvares Group. What
I learned from public material (company site, product pages, Microsoft's Dynamics 365 announcement, the UK G-Cloud
listing, case studies):

| fact | consequence for the matrix service |
|---|---|
| Product **VISITOUR**: real-time appointment booking, route optimisation with the "PowerOpt" algorithm, continuous in-day re-optimisation, self-service online booking (e.g. HomeServe) | Matrices are requested *often* and on the *critical path of an interactive action* (a customer on the phone or on a web page waiting for slots). Latency matters more than throughput. Most traffic will be incremental: a few new locations against an existing plan. |
| Customers are **field-service operations**: HVAC and boiler engineers, telecoms, utilities (E.ON), elevator service (TKE), housing repairs, home-care clinicians, surveyors; "50 to 5,000 technicians", "thousands of orders" | Vehicles are **cars and vans**, not trucks: a car profile is the right model. Instances are regional (a technician drives 20–60 min between jobs, not 1,000 km). Jobs last 30 min – several hours with appointment windows of hours, so travel time is a minority of the working day. |
| Offices in **Germany, the UK and the Netherlands**; the group serves customers in 45 countries | Europe (and within it DE, GB, NL) carries the load, but "whole world" is a real requirement, not a formality. |
| Hosted on **AWS** (G-Cloud listing: AWS, UK data residency, private cloud, one environment per customer) | The client of this service is an optimiser *inside AWS*. "Time to last byte at the client" is therefore an in-region, in-VPC measurement. That is the only setting in which 100 ms for a 1,000² matrix is physically possible (see §2). |
| VISITOUR advertises "exact geocoding", "time-of-day-dependent road profiles" and "predictive traffic-based driving speeds" | The optimiser already owns a traffic model and applies it on top of a base network. The matrix service must be a *fast, stable, consistent free-flow base*; it does not need, and cannot get from OpenStreetMap, live traffic. |

What the stated requirements really stand for:

* **"1,000 × 1,000 in 100 ms"** — interactive re-optimisation and slot search must never wait on the matrix. The real
  requirement is *predictable* low latency (p99, not p50) under concurrent use, for 1,000-point requests and anything
  smaller.
* **"Whole world"** — any customer, anywhere, must get answers without a per-region deployment. It does not mean
  cross-continent routes must be fast; they must merely be correct (usually "no route").
* **"Asymmetric, one-ways, turn restrictions"** — a VRP must not be fed a symmetric approximation that plans a
  technician the wrong way down a one-way street. The customer needs *directionally correct* times.
* **"Replaces an unstable legacy service"** — stability, bounded memory, overload behaviour and operability are
  requirements with equal weight to speed.

## 2. Why the problem is hard (measured)

The budget is 100 ms for 1,000,000 routes *including* transfer and client parsing, on the whole-world road graph.

| obstacle | measurement | what it rules out |
|---|---|---|
| **Size of the world graph** | the car-routable OSM planet (2026-09-21) is 148 M ways, 1.64 B node references; after collapsing degree-2 nodes 235 M junctions and 297 M road chains, 1.9 M turn restrictions | anything that is not a preprocessed speed-up technique; a plain Dijkstra from one source over Eurasia settles ~150 M nodes |
| **Routes per request** | 1,000,000 per 1,000-point request, i.e. 100 ns per route end to end | one query per pair, even with a fast point-to-point technique (CH queries are ~100 µs → 100 s) |
| **An off-the-shelf engine** | OSRM's CH `table` service, same machine, on a *city* extract (Hamburg, 365 k nodes): **1,126 ms** for 1,000 × 1,000, returning 14.7 MB of JSON | reusing an existing engine as is |
| **Response size** | 1,000² × (distance, time) = 2 M integers: **10.7 MB** as JSON, **8.0 MB** as binary u32 | nothing — but it has to be moved and parsed within the same 100 ms |
| **Client-side parsing** | JSON → Python objects: **188 ms**; JSON → typed Rust matrices (serde): **33 ms**; binary → numpy view: **2 µs** | JSON as the fast path: a Python client alone blows the whole budget parsing it |
| **The network** | a single TCP flow between EC2 instances outside a placement group is capped at 5 Gbit/s: 8 MB ≥ 13 ms; from outside AWS (e.g. a 100 Mbit/s office line) ≥ 640 ms plus RTT | any client that is not in the same region as the service |
| **Memory** | c5a.4xlarge has 32 GB; a 1,000² request touches tens of thousands of scattered graph pages, and an EBS read costs from hundreds of µs to milliseconds, so the world graph must stay resident | uncompressed edge-based graphs (OSRM-style) for the planet; per-region shards on one box |

So the problem is not one hard thing but five budgets that each consume most of the 100 ms if handled naively:
graph search, matrix assembly, encoding, transfer and parsing. Each has to be driven down to a few milliseconds.


## 3. How the objective was reshaped

The service computes **exact** fastest routes on its road model, for the **whole world**, **asymmetric**,
**deterministic**. Those are not negotiable: an optimiser that receives wrong-way routes, different answers for the
same question, or holes in coverage produces bad plans that nobody can explain. What was reshaped is *where* the
100 ms is measured, *how* the result is shipped, and *which road-model details* are worth their cost. Every relaxation
below was measured against what the optimiser downstream actually does with the numbers.

**How much matrix error does a VISITOUR-like optimiser tolerate?** I built field-service instances in Hamburg
(30 technicians starting from home, 150 jobs of 30–90 min in AM / PM / all-day windows, 08:00–18:00 shifts),
optimised them with PyVRP (a state-of-the-art hybrid genetic search VRP solver) using different planning matrices, and
re-timed every resulting plan on a reference matrix (OSRM with its turn-penalty model). 4 instances × 4 seeds × 30 s.

| planning matrix | real travel time of the plan vs. truth-planned | seed-to-seed spread | lateness per plan under truth | plans with zero lateness |
|---|---|---|---|---|
| truth (OSRM with turn delays) | ±0 % | 0.11 % | 0.0 min | 100 % |
| **ours** | **−0.25 %** | 1.04 % | **4.2 min** (≈ 8 s per technician-day) | 31 % |
| ours × 1.015 (median bias removed) | −0.08 % | 1.07 % | 2.8 min | 31 % |
| truth with each cell × lognormal noise 1 % | −0.00 % | 0.33 % | 0.0 min | 94 % |
| … noise 2 % | +0.13 % | 0.18 % | 0.0 min | 100 % |
| … noise 5 % | +0.51 % | 0.38 % | 0.1 min | 69 % |
| … noise 10 % | +1.71 % | 0.98 % | 4.3 min | 19 % |
| … noise 20 % | +7.28 % | 1.53 % | 13.5 min | 0 % |
| truth symmetrised, (A→B + B→A)/2 | +0.91 % | 0.60 % | 0.1 min | 81 % |
| straight-line distance / median speed | +5.95 % | 4.86 % | 46.9 min | 0 % |

Two things matter to a field-service operator: how much driving the plan costs and whether it keeps appointments. Plan
efficiency is insensitive to matrix noise up to ~5 % (the difference is inside the solver's own seed-to-seed spread);
at 10 % it costs ~2 %, at 20 % ~7 % and appointments start to be missed. Straight-line estimates — the classical
shortcut — cost 6 % and ~50 min of lateness per plan. Ignoring asymmetry costs ~1 %. That sets the bar: **errors of a few
percent in individual cells are harmless; systematic optimism and >10 % errors are not.**

| # | relaxation | what is lost | why acceptable here | how it was measured |
|---|---|---|---|---|
| R1 | The 100 ms target applies to a client **in the same AWS region**, over a **persistent connection** | clients outside AWS get correct answers but cannot get them in 100 ms | VISITOUR is hosted on AWS; the consumer is an optimiser process, not a browser. Physics: 8 MB takes ≥13 ms on a 5 Gbit/s flow, ≥640 ms on a 100 Mbit/s office line | §6: time-to-last-byte from a separate EC2 instance |
| R2 | **Binary response format** is the fast path; JSON is served as specified but is not the 100 ms path for 1,000² | clients need a 1-line decoder (`np.frombuffer(...)`) | parsing 10.7 MB of JSON takes 188 ms in Python and 33 ms in Rust; the binary body is a zero-copy view (2 µs) | §2 |
| R3 | **Turn restrictions and one-ways are modelled exactly; turn *delays* (seconds spent turning) are not** | durations ~1.4 % lower than OSRM's turn-delay model in a city (median), ~5 % lower for the most turn-heavy tenth of pairs; in the VRP test ~4 min of lateness per 30-technician plan under that model, i.e. ~8 s per technician-day | plan efficiency is unchanged (within solver noise); VISITOUR applies its own time-of-day speed model on top, whose corrections are an order of magnitude larger; a turn-delay model needs an edge-based graph, 2–3× the memory — the whole world would no longer fit in 32 GB — and larger search spaces | §5 table 4 (with / without OSRM turn penalties), VRP table |
| R4 | **Free-flow speeds** from OSM (OSRM's car defaults; 80 % of posted limits; unpaved ≤ 30 km/h; 2 s per traffic signal) | no congestion, no time of day | OSM has no traffic; the customer already owns a predictive traffic layer | — |
| R5 | **Snapping**: nearest road in a connected network within 5 km, on geometry simplified to 5 m; driveways, parking aisles, private roads and service roads open only to destination/delivery/customer traffic are not part of the graph; the straight line from the point to its road is charged at 15 km/h, its length added to the distance | the true shape and speed of the last metres (an unmapped driveway, a farm track, a car park) | for an address the leg is a few seconds; for a farm or a site at the end of a private track it is the minute or two a technician really needs, instead of zero; keeping private fragments out of the graph stops addresses snapping into dead-end networks and routes cutting through car parks (the main source of disagreement with OSRM, §5) | §5 tail analysis, §5 the task's example |
| R6 | **Ferries and car trains** only when tagged for cars; time from `duration`, else `maxspeed`, else 20 km/h; no timetable waiting | ±15 min on island trips that depend on sailing schedules | islands are a small share of field-service work; the schedule is not in OSM anyway | Sylt/Amrum analysis, §5 |
| R7 | **Size envelope**: ≤ 25,000 locations and ≤ 100 M cells per binary request (10k × 10k), ≤ 16 M cells per JSON request; larger matrices are tiled by the client with `sources`/`destinations` | a single call cannot return a 20k × 20k matrix (3.2 GB) | above ~2,000 points the response is bandwidth-bound; tiling lets a client parallelise or cache | §6 |
| R8 | **Weekly data refresh** from the planet file, ~1 h on one spot instance | road changes appear with up to a week's delay | the road network changes slowly relative to planning horizons of hours to days | build logs |

**Additions the stated spec did not ask for but the business needs:** `sources`/`destinations` for incremental
updates — when one new job is booked into a plan of 1,000 stops, the optimiser needs a 1 × 1,001 row and a 1,001 × 1
column, which cost 9–14 ms (measured, §6) instead of 31–36 ms for the full 10⁶-cell matrix; `null` for genuinely unroutable pairs instead of a large
fake number, so a bad geocode or a car-free island is visible instead of silently planned; per-point snapping failures
are counted in the metrics (`dm_unsnapped_points_total`) so geocoding problems are observable.

**What "works well" means per size** (targets, in-region client, binary):

| request | use in VISITOUR | target | result |
|---|---|---|---|
| 1 × N, N × 1 (booking a slot) | interactive, on the phone | ≤ 20 ms for N ≤ 1,000 | 9–14 ms at N = 1,000; 36–75 ms at N = 5,000 |
| ≤ 100 points | small plans, re-optimisation of one region | < 10 ms | ≤ 5 ms p99 |
| 1,000 points | the stated requirement | p99 < 100 ms | 31–36 ms p50, < 40 ms p99 anywhere in the world; + 2.6 ms client decode |
| 2,000–5,000 points | daily plans of a large region | < 1 s, bounded memory | 2,000²: 80–100 ms; 5,000²: 0.5 s (bandwidth-bound) |
| 10,000 points | national day plan | seconds, bounded memory, no effect on other clients | 2.0 s (800 MB); interactive requests keep p99 < 100 ms meanwhile |


## 4. Architecture

```
 OSM planet PBF ──► dm-build (offline, 1 large spot instance, ~1 h)                    S3 (versioned datasets)
   1. parallel PBF passes: routable ways + turn restrictions, then only referenced nodes        │
   2. car/van profile: access, oneway, speeds from maxspeed/road class, barriers, signals,        │
      ferries and car trains (duration / maxspeed)                                               │
   3. topology: compress degree-2 chains, split at blocking barriers and loops                    │
   4. split only the junctions that carry turn restrictions (one copy per restricted approach)   │
   5. strongly connected components (major ≥ 1,000 nodes vs. islands / data errors)             │
   6. contraction hierarchy: parallel independent-set contraction, lexicographic (time, dist)   │
   7. assemble: rank-ordered CSR arcs grouped by direction, Hilbert-ordered chains, packed       │
      Hilbert R-trees for snapping, Douglas–Peucker geometry (5 m) ──────────────────────────────┘
                                                                                                  ▼
 dm-server (c5a.4xlarge): mmap + pre-fault + mlock the flat arrays, validate, serve
   POST /matrix ─► validate ─► admission (cells in flight, bounded FIFO queue, timeout → 503)
                ─► lane by size (≤ 250 k cells / ≤ 4 M / larger), each FIFO with its own thread pool:
                               snap all points (R-tree, parallel)
                               backward CH searches from all targets → bucket entries
                               sort by node → sparse buckets + dense SIMD rows for hot nodes
                               for each block of rows: forward CH searches, scan buckets,
                                   same-chain shortcuts, → u32 metres / seconds
                ─► stream blocks as they finish (binary), or encode JSON in parallel
```

**Weights.** Travel time (ms) and distance (dm) are packed into one `u64` as `time << 32 | distance`. Adding and
comparing packed values is exactly lexicographic comparison of (time, distance), so the whole hierarchy computes the
fastest route and breaks ties by the shorter distance. Results are deterministic and independent of thread scheduling,
and the reported distance is always the distance *of* the fastest route.

**Graph.** A node-based graph of junctions (degree-2 chains collapsed), which keeps the planet at 235 M nodes and
297 M chains. Turn restrictions (1.9 M in the planet) are modelled exactly by copying only the restricted junctions:
the copy receives the restricted approach and keeps only the permitted exits, and points on the affected chains get the
matching seed nodes. This costs 0.6 % extra nodes instead of the 2–3× blow-up of a fully edge-based graph.

**Many-to-many.** Bucket-based CH many-to-many (Knopp et al.). With stall-on-demand an upward search settles about
110 nodes on the Germany graph and about 200 on the world graph; for 1,000 clustered points 98–99 % of all bucket work
lands on the few hundred nodes at the top of the hierarchy that almost every target reaches (275 for Germany, 411 for
the world). Those buckets are stored as dense rows (one weight per target) and
relaxed with an explicit AVX2 compare-and-blend kernel. LLVM's auto-vectorised loop used masked stores (`vpmaskmovq`),
which are microcoded on Zen 2; replacing them cut the row phase from 9.6 to 7.1 ms at 1,000² and from 135 to 74 ms at
5,000² (Germany graph). The rest stay sparse. Rows are independent, so they are computed in
parallel blocks and streamed.

**Snapping.** Each coordinate snaps to the nearest road segment in a *major* strongly-connected component (≥ 1,000
nodes) within 5 km. A road in a minor component (a car-free island's network, a one-way data error that forms a sink)
wins only if it is more than 1 km closer than the nearest major road. The point is placed at its fraction along the
chain; forward/backward seeds carry the partial chain cost, and two points on the same chain are also connected
directly along it. The coordinate-to-road offset is not added (it is part of the service time, as in OSRM).

**Wire format.** JSON exactly as specified, plus a binary format: 16-byte header then, per row, `u32` distances and
`u32` times, little-endian — a zero-copy view for the client. Rows are streamed as soon as each block is done, with an
exact `Content-Length`, so network transfer overlaps the computation of later rows.

**Robustness.** Datasets are immutable flat arrays with a manifest, validated at load (sizes, sorted/grouped
invariants) and locked in RAM so that request memory pressure can never evict them. Requests are admitted by the number
of matrix cells in flight, queue in FIFO order with a timeout and bounded queue length, and fail fast with `503 +
Retry-After` instead of degrading everyone. They then run one at a time in one of three size lanes, each with its own
thread pool, so requests cannot bury each other inside the work-stealing scheduler and a batch job cannot block an
interactive one (§6). A panic in a computation is caught and turns into a failed response, not a
dead process. Client disconnects stop the computation. Graceful shutdown drains in-flight requests.


## 5. Correctness evidence

Three independent layers, each run on real data.

**1. Unit and property tests** (`cargo test --workspace`, 44 tests including the end-to-end ones below). The contraction hierarchy is compared against
Dijkstra for *all pairs* of random directed graphs with one-way and asymmetric arcs, with both tight and generous witness
limits; the packed R-tree against brute force; the AVX2 kernel against scalar code; the open-addressing map against
`HashMap`; tag handling of the vehicle profile (access, oneway, maxspeed formats, ferries, car trains, barriers, turn
restrictions); junction splitting (a forbidden turn disappears only for the restricted approach, an `only_` turn keeps
only its exit, malformed restrictions are skipped); the wire format both ways.

**2. End-to-end tests** (14, `crates/dm-server/src/tests.rs`). A dataset is built from a real OSM extract of central
Kiel and served over HTTP, in-process and over TCP: JSON has exactly the specified shape, binary and JSON agree cell by
cell, one-way streets make the matrix asymmetric, rectangular requests equal the corresponding square cells, duplicate
points are free, a point off the road pays its way to the road and back, points 50 km out to sea are `null`, every malformed request gets the right 4xx with an explanation,
overload returns `503` + `Retry-After`, metrics are exported, a 400 × 400 matrix streams with an exact `Content-Length`.

**3. Dataset verification against an independent reference** (`dm-bench verify`). For random realistic points the
engine's matrix is compared with a plain Dijkstra over the *uncontracted* graph, in which every query point is inserted
as a virtual node splitting its road (so snapping offsets and same-road pairs are checked too), including the junction
copies for turn restrictions. Also checked: zero diagonal, the triangle inequality on all triples of 150 points,
bit-identical results on repetition, and consistency under permutation of the input.

| dataset | points | pairs checked against Dijkstra | exact | worst deviation | reachability disagreements | invariant violations |
|---|---|---|---|---|---|---|
| Schleswig-Holstein | 401 regional | 40,100 | 40,092 | 1 m (rounding) | 0 | 0 |
| Germany | 501 in Hamburg | 12,024 | 12,020 | 1 m | 0 | 0 |
| Germany | 501 Germany-wide | 12,024 | 12,024 | 0 | 0 | 0 |
| Hamburg city extract (with turn restrictions) | 601 | 24,040 | 24,029 | 1 m | 0 | 0 |
| **planet** | 401 in London | 9,624 | 9,623 | 1 m | 0 | 0 |
| **planet** | 401 in Hamburg | 9,624 | 9,624 | 0 | 0 | 0 |
| **planet** | 301 across Great Britain | 3,612 | 3,612 | 0 | 0 | 0 |
| **planet** | 201 across Europe | 804 | 804 | 0 | 0 | 0 |
| **planet, car-v2** | 401 in London | 9,648 | 9,648 | 0 | 0 | 0 |
| **planet, car-v2** | 401 in Hamburg | 9,648 | 9,648 | 0 | 0 | 0 |

**4. Model validation against OSRM** (`dm-bench compare-osrm`, OSRM v5 car profile as an independent engine on the
same OSM extract, 900 random building locations, ~268 k pairs per row; pairs under 60 s or 500 m excluded).

| region | reference | duration ours/OSRM (p10 · median · p90) | within 5 % | within 10 % | median abs. error |
|---|---|---|---|---|---|
| Schleswig-Holstein | OSRM default | 0.970 · 0.989 · 0.996 | 96.7 % | 99.7 % | 48 s |
| Hamburg | OSRM default | 0.950 · 0.986 · 1.001 | 89.8 % | 97.5 % | 22 s |
| Hamburg | OSRM without turn penalties | 0.973 · 1.003 · 1.017 | 94.8 % | 98.5 % | 14 s |

Distances: median ratio 0.997–0.998 everywhere. Reading the table: with OSRM's turn penalties switched off the median
bias disappears, so the ~1.4 % gap in cities is OSRM's turn-delay model (7.5 s sigmoid by angle, 20 s U-turn). In the
tail, the two engines disagree about *which road an address attaches to*: for the two most frequent outliers OSRM
itself prices our route cheaper than the one it returned (1,783 s vs 2,170 s; 2,286 s vs 2,536 s), because it snapped
the building to a cul-de-sac or park road reachable only by a detour.

**5. The task's own example.** The task statement shows an illustrative response for three points near Neumünster
(54.0/10.0, 54.1/10.1, 54.2/10.4). Four engines on the same request (distance km / time min; OSRM and Valhalla are their
public demo servers with their own data snapshots):

| cell | task, "illustrative" | ours, car-v1 | **ours, car-v2** | OSRM | Valhalla |
|---|---|---|---|---|---|
| 0→1 | 18.2 / 22.0 | 17.5 / 18.0 | **18.1 / 20.2** | 18.7 / 21.2 | 18.6 / 32.2 |
| 0→2 | 55.9 / 52.0 | 48.0 / 47.7 | **48.9 / 51.3** | 47.8 / 54.5 | 53.0 / 69.8 |
| 1→0 | 18.2 / 22.4 | 17.5 / 17.8 | **18.1 / 20.0** | 18.7 / 21.0 | 18.7 / 33.8 |
| 1→2 | 32.4 / 38.0 | 35.2 / 41.0 | **36.0 / 44.4** | 31.9 / 46.3 | 34.2 / 52.3 |
| 2→0 | 61.4 / 56.9 | 48.0 / 47.6 | **48.9 / 51.2** | 47.8 / 54.5 | 64.4 / 72.1 |
| 2→1 | 38.2 / 44.5 | 35.2 / 41.0 | **36.0 / 44.4** | 31.9 / 46.4 | 34.2 / 52.1 |

The mature engines disagree with each other by up to 50 % on these cells, so the example is no reference. The useful
question was why car-v1 was 12–16 % faster than OSRM here when it is ~1.5 % faster on addresses (table 4). I traced the
routes: a road point Q lies on our A→B route exactly when t(A,Q) + t(Q,B) = t(A,B), so probing every OSM road node in
the area reconstructs our route; pricing it in OSRM through waypoints, and comparing cumulative times along OSRM's own
route, locates every second of difference.

* **0→2:** the routes are identical for 44.5 km and agree within 10 s. All 409 s of difference are OSRM's last 582 m: to
  reach point 2, which lies in a field, it drives a grade-2 farm track at 5 km/h. We do not route tracks and attached the
  point to the public road instead — for free.
* **0→1:** identical for 17 km; OSRM adds 33 s of turn penalties there (relaxation R3, 3 %). The other 162 s come from
  attaching point 1 (250 m from any road) to a different road.
* **0↔2** also cut through a service road tagged `access=customers` (a car park), which OSRM allows only at the start
  or end of a route.

A wider sample confirms the mechanism (80 random points per sample fed to both engines at OSRM's own snapped positions;
pairs ≥ 5 km):

| sample | car-v1: median ours/OSRM · pairs > 10 % faster | car-v2 |
|---|---|---|
| rural Schleswig-Holstein, random points (mostly in fields) | 0.965 · 23.0 % | 0.975 · 12.2 % |
| rural Schleswig-Holstein, points on named public roads (like addresses) | 0.987 · 5.0 % | 0.987 · 3.7 % |
| Hamburg | 0.975 · 4.8 % | 0.980 · 3.6 % |

For address-like points the countryside behaves like the city; the tail belonged to points far from any road, where
car-v1 charged nothing for the way to the road. Release **car-v2** therefore (a) charges the straight line from a point
to its road at 15 km/h and adds its length to the distance (a few seconds for an address, 1–2 min for a farm at the end
of a track), and (b) removes service roads open only to destination, delivery or customer traffic from the graph. On
the example car-v2 lands within 4–6 % of OSRM in every cell. Residential streets tagged "destination only" stay
routable (addresses are on them); that through traffic may use them is a remaining relaxation of the node-based model.

**Bugs found by this process, all fixed:** ferries tagged only `hgv=yes` were excluded (Kiel Canal ferry Hohenhörn);
car trains ignored `maxspeed` and `oneway` (Sylt Shuttle at 20 km/h → 89 min too slow to Sylt); a stale dataset
without grouped arcs was correctly refused at load. One disagreement is OSRM's error, not ours: OSRM routes cars to
Helgoland, a car-free island, over a passenger ferry; we return `null`.


## 6. Performance evidence

**Setup.** Server: the production host, an on-demand **c5a.4xlarge** in us-east-1f serving the **whole-world dataset**
(`planet-260921`: 236.6 M nodes, 590 M hierarchy arcs, 19 GB locked in RAM). Client: a second c5a.4xlarge in the same
availability zone — the position of an optimiser in the same VPC. Measured RTT 0.16 ms; one TCP flow carries
4.96 Gbit/s (AWS caps a single flow outside a placement group at 5 Gbit/s; 4 flows reach 9.9). One persistent
HTTP/1.1 connection, binary format unless stated. Every request uses a *different* random sample of realistic
locations (buildings and address points from OSM for the European scenarios, road junctions elsewhere); 300 requests
per row after 5 warm-ups. **TTLB** is from the first byte of the request to the last byte of the response at the
client; **client decode** is turning the binary body into two owned `u32` matrices (a zero-copy view costs ~0).

**The requirement — 1,000 × 1,000 (1 M routes), binary:**

| scenario | points from | p50 | p90 | p99 | max | + client decode |
|---|---|---|---|---|---|---|
| hamburg | OSM buildings/addresses, Hamburg | 30.6 ms | 35.3 ms | **36.4 ms** | 37.4 ms | 1.7 ms |
| london | OSM buildings/addresses, Greater London | 33.7 ms | 36.0 ms | **36.9 ms** | 37.4 ms | 2.6 ms |
| schleswig-holstein | same, Schleswig-Holstein (the spec's example region) | 33.3 ms | 36.4 ms | **37.3 ms** | 38.6 ms | 2.5 ms |
| netherlands | same, whole Netherlands | 33.8 ms | 38.2 ms | **39.1 ms** | 40.2 ms | 1.9 ms |
| great-britain | same, whole Great Britain | 34.5 ms | 37.0 ms | **37.8 ms** | 38.6 ms | 2.6 ms |
| germany | same, whole Germany | 36.0 ms | 38.8 ms | **39.8 ms** | 40.2 ms | 2.6 ms |
| europe | same, whole of Europe | 35.4 ms | 38.9 ms | **39.8 ms** | 40.1 ms | 2.5 ms |

The whole-world graph answers a 1,000 × 1,000 matrix in **31–36 ms median and under 40 ms at p99** from any of these
regions — 2.5× inside the budget including client decoding. Where the time goes (Hamburg, measured in-process on the
server): snapping 1 ms, backward searches and buckets 12 ms, forward searches and bucket scans 12 ms. An upward search
settles ~199 nodes on the world graph (110 on Germany alone); 99.4 % of the bucket work hits the 411 dense rows. The
first block of rows leaves the server after ~13 ms and transfer (≥ 13 ms for 8 MB on one flow) overlaps the rest of
the computation.

**Other sizes:**

| points | response | Hamburg p50 / p99 | Europe-wide p50 / p99 |
|---|---|---|---|
| 10 × 10 | 816 B | 1.5 / 1.6 ms | 1.5 / 1.7 ms |
| 100 × 100 | 80 kB | 3.6 / 4.1 ms | 4.2 / 4.8 ms |
| 500 × 500 | 2.0 MB | 13.6 / 15.2 ms | 14.9 / 16.1 ms |
| 1,000 × 1,000 | 8.0 MB | 30.6 / 36.4 ms | 35.4 / 39.8 ms |
| 2,000 × 2,000 | 32.0 MB | 79.1 / 97.4 ms | 87.9 / 100.7 ms |
| 5,000 × 5,000 | 200.0 MB | 486.3 / 502.3 ms | 490.7 / 511.9 ms |
| 10,000 × 10,000 | 800 MB | 1.99 / 2.00 s | — |

Up to 1,000 points the service is compute-bound and fast; from ~2,000 points it is **bandwidth-bound**: a 5,000²
matrix is 200 MB, which a single 5 Gbit/s flow needs 320 ms to carry, and decoding it takes the client 120 ms. Server
memory stays bounded because rows are streamed. Clients that need large matrices faster can fetch row bands in
parallel on several connections with `sources`.

**Other continents** (1,000 points sampled from road junctions inside each region; `world-mix` mixes eight cities on
five continents, where 56 % of pairs are correctly `null`):

| region | 100 × 100 p99 | 1,000 × 1,000 p50 / p99 |
|---|---|---|
| new-york | 4.2 ms | 27.8 / 31.1 ms |
| los-angeles | 3.6 ms | 29.5 / 32.5 ms |
| usa | 4.1 ms | 32.3 / 36.6 ms |
| sao-paulo | 3.3 ms | 23.9 / 29.5 ms |
| tokyo | 5.1 ms | 43.3 / 45.5 ms |
| sydney | 2.4 ms | 21.3 / 26.7 ms |
| nairobi | 3.5 ms | 28.9 / 32.4 ms |
| mumbai | 3.7 ms | 29.1 / 34.2 ms |
| world-mix | 4.0 ms | 31.6 / 35.9 ms |

**JSON** (same requests): the server is fast, but the client pays for parsing — with a fast Rust parser the 1,000²
total is just under 100 ms; a Python client spends 188 ms in `json.loads` alone. Binary is the fast path.

| points | JSON body | TTLB p50 / p99 | client parse (Rust, serde) | total p99 |
|---|---|---|---|---|
| 10 × 10 | 0.00 MB | 1.8 / 1.9 ms | 0.0 ms | 1.9 ms |
| 100 × 100 | 0.11 MB | 3.8 / 4.4 ms | 0.4 ms | 5.1 ms |
| 500 × 500 | 2.66 MB | 16.9 / 18.4 ms | 9.0 ms | 28.4 ms |
| 1,000 × 1,000 | 10.65 MB | 44.7 / 52.8 ms | 34.2 ms | 92.7 ms |

**Booking a new job into an existing plan** (the real-time VISITOUR operation, `sources` / `destinations`):

| request | Hamburg p50 / p99 | Europe-wide p50 / p99 |
|---|---|---|
| 1 × N (new job → all stops), N = 100 | 2.1 / 2.5 ms | 2.4 / 3.0 ms |
| 1 × N (new job → all stops), N = 1,000 | 10.6 / 11.5 ms | 13.5 / 14.4 ms |
| 1 × N (new job → all stops), N = 5,000 | 49.0 / 75.2 ms | 60.3 / 67.4 ms |
| N × 1 (all stops → new job), N = 100 | 1.8 / 2.3 ms | 2.0 / 2.4 ms |
| N × 1 (all stops → new job), N = 1,000 | 8.5 / 8.8 ms | 10.4 / 11.1 ms |
| N × 1 (all stops → new job), N = 5,000 | 36.2 / 37.3 ms | 44.3 / 46.4 ms |

**Concurrent load** (1,000 × 1,000 and 100 × 100 matrices from several clients at once):

| request | concurrent clients | p50 | p99 | max | throughput |
|---|---|---|---|---|---|
| 1,000 × 1,000 | 1 | 31 ms | 35 ms | 36 ms | 30 req/s |
| 1,000 × 1,000 | 2 | 54 ms | 60 ms | 61 ms | 36 req/s |
| 1,000 × 1,000 | 4 | 110 ms | 115 ms | 117 ms | 36 req/s |
| 1,000 × 1,000 | 8 | 220 ms | 227 ms | 229 ms | 36 req/s |
| 1,000 × 1,000 | 16 | 443 ms | 451 ms | 453 ms | 36 req/s |
| 100 × 100 | 1 | 4 ms | 4 ms | 5 ms | 274 req/s |
| 100 × 100 | 8 | 26 ms | 28 ms | 28 ms | 307 req/s |
| 100 × 100 | 32 | 104 ms | 109 ms | 319 ms | 305 req/s |

Latency under load grows linearly with the queue (FIFO) and has no long tail. The first design ran all requests on
one shared work-stealing pool: throughput was similar (~40 req/s) but a worker waiting inside one request's parallel
join would pick up another request's root task and bury the first under it — at 16 clients the p99 was 2.7 s and the
worst request took 3.6 s, and even 100-point requests saw 3.5 s. Requests now run one at a time per lane, in three
lanes with their own thread pools (≤ 250 k cells, ≤ 4 M cells, larger), so a 10,000² batch job cannot stall
interactive work. With 10,000² jobs running back to back, interactive 1,000² requests measured p50
46 ms / p99 75 ms and 100² requests p99 8.4 ms. One host
sustains ~36 matrices of 1,000² per second (36 M routes/s) or ~300 of 100²; beyond that, add hosts.

**Baseline.** OSRM's CH table service on the same hardware takes 1,126 ms for a 1,000 × 1,000 table on a *Hamburg
city extract* (365 k nodes); this service takes 25–30 ms of compute for the same size on the *world* graph.

**Re-measured on the car-v2 release** (same host, client in the same availability zone, RTT 0.21 ms; building
locations from current Geofabrik extracts, road junctions for Europe; results in `results/car-v2/`):

| request | car-v1 p50 / p99 | car-v2 p50 / p99 |
|---|---|---|
| 1,000² Hamburg | 30.6 / 36.4 ms | 27.6 / 32.2 ms |
| 1,000² London | 33.7 / 36.9 ms | 30.4 / 35.0 ms |
| 1,000² Schleswig-Holstein | 33.3 / 37.3 ms | 30.2 / 34.3 ms |
| 1,000² Europe-wide (car-v2: road junctions) | 35.4 / 39.8 ms | 32.2 / 37.0 ms |
| 10² / 100² / 500² Hamburg | 1.5 / 1.6 · 3.6 / 4.1 · 13.6 / 15.2 ms | 1.7 / 1.9 · 3.4 / 3.8 · 12.8 / 14.0 ms |
| 2,000² / 5,000² Hamburg | 79.1 / 97.4 · 486 / 502 ms | 75.8 / 90.6 · 471 / 475 ms |
| 1 × 1,000 / 1,000 × 1 Hamburg | 10.6 / 11.5 · 8.5 / 8.8 ms | 10.1 / 11.2 · 7.6 / 8.0 ms |
| 1,000² JSON Hamburg | 44.7 / 52.8 ms | 42.3 / 47.3 ms |
| 1,000² via `bench/try_api.py` (Python, standard library, incl. decoding) | — | 32.0 / 33.5 ms; 32.5 / 33.9 ms through the public address |

Charging the access leg costs nothing measurable; the small gains come from the slightly smaller graph (235.5 M nodes,
587 M arcs).


## 7. Operations

**Deployment shape.** One stateless process per host (`dm-server`, systemd, hardened unit), one immutable dataset
directory per OSM snapshot under `/opt/dm/data/<version>`, an atomic `current` symlink, and the binary and datasets in a
private S3 bucket. A new host is `infra/bootstrap-server.sh` as EC2 user data: it runs `infra/deploy/upgrade.sh`, which pulls
the release binary and the dataset and runs `infra/deploy/install.sh`, which installs the unit, applies kernel settings (no TCP slow-start after idle, large
socket buffers) and waits for `/health`. Horizontal scaling and zero-downtime updates are "more of the same host behind
a load balancer"; there is no shared state.

**Memory footprint.** The world dataset is 19.4 GB of flat arrays: hierarchy arcs 7.1 GB, road chains (endpoints, costs, geometry offsets) 7.1 GB, simplified geometry 2.5 GB, node coordinates 1.9 GB, arc offsets 0.9 GB, spatial index 0.3 GB, turn tables 33 MB. All of it is locked in RAM, which leaves ~11 GB on a c5a.4xlarge for request buffers (the admission limit of 200 M cells in flight bounds them at 1.6 GB of `u32` output plus bucket structures).

**Startup.** Warm restart (dataset in page cache): 2.7 s to serving. Cold boot of the host: 101 s from reboot to healthy, of which 82 s is reading 19 GB from gp3 at ~240 MB/s. A brand-new host goes from launch to serving in under 2 minutes (the dataset is pulled from S3 at ~600 MB/s).

**Data refresh.** `infra/build-dataset.sh` launches a one-off spot instance (r6a.4xlarge, 128 GB) with
`infra/bootstrap-builder.sh`: it takes the newest planet from the AWS open-data bucket, runs `dm-build` (51 min for the planet on 16 cores: 6 min ways and restrictions, 6 min nodes, 8 min topology, 3 min components, 30 min contraction, 2 min writing; the last run peaked at 112 GB, which is why geometry is now simplified before contraction and every phase logs its peak memory),
uploads `datasets/planet-YYMMDD` and its build log to S3 and terminates itself — about $0.50 per build. The car-v2
build of the same planet took 48 min and peaked at 93 GB. On a serving
host, `infra/deploy/upgrade.sh planet-YYMMDD` downloads the release binary and the dataset, switches the symlink,
restarts, and keeps one previous dataset for rollback; the previous release stays in S3 under `releases/car-v1/`. Weekly is the recommended cadence; a restart makes a single host unavailable for the load time, so
production should run two hosts behind a load balancer and update them one at a time.

**Overload and failure behaviour.**

| situation | behaviour |
|---|---|
| more work than capacity | requests queue FIFO by matrix cells in flight (default 200 M cells); beyond 256 queued or 10 s wait → `503 Retry-After: 1` |
| request too large | `413` with the limit and a hint (binary format, tiling with `sources`/`destinations`) |
| malformed input, out-of-range coordinates, unknown fields | `400` with the reason; nothing is computed |
| coordinate far from any road (sea, wrong geocode) | row/column `null`, counted in `dm_unsnapped_points_total` |
| client disconnects mid-stream | computation stops at the next block, capacity is released |
| panic inside a computation | caught; the request fails (500 or truncated body with exact `Content-Length`), the process keeps serving |
| memory pressure from large requests | dataset is `mlock`ed and cannot be evicted; admission bounds request buffers |
| corrupted or incompatible dataset | refused at startup (manifest version, array sizes, sorted/grouped invariants) |
| SIGTERM | stops accepting, drains in-flight requests, exits |

**Observability.** Every matrix response carries a `Server-Timing` header (queue wait; for JSON also compute time).
Structured JSON logs (one line per computed matrix with size, format and compute time; one per
rejection with reason). Prometheus metrics: request counts by status and format, compute-time and queue-wait
histograms by size class, cells in flight, queued requests, unsnapped points, loaded dataset. Suggested alerts: p99
`dm_compute_seconds` for the 1,000² class above 50 ms, any `503`, `dm_unsnapped_points_total` rate jumps (a customer's
geocoder broke), dataset older than 14 days.

**Security.** The service has no authentication by design of the spec; in production it belongs in a private subnet
reachable only from the optimiser's security group. The demo instance is deliberately public (port 8080 open to the
internet, fixed address 100.57.61.188) so that it can be tested; the request limits and admission control above keep it
stable under abuse, but there is no per-client quota, so one heavy client can make others queue. For TLS or cross-VPC access put an internal ALB/NLB in front (TLS adds ~2–4 ms on
8 MB).


## 8. What I would do next

* **Turn delays for Europe only.** An edge-based graph for the continent where the customers are would fit next to
  the world dataset on a larger instance and remove the ~1.4 % city bias; the VRP experiment says it is not urgent.
* **Time-of-day profiles.** VISITOUR already has them; a customizable CH (same topology, several metrics) could serve
  one matrix per time slice with a metric update in seconds.
* **Booking at ~1 ms.** A 1 × 1,000 request spends its 10 ms on the 1,000 searches of the existing stops; caching
  per-point search spaces across requests would remove them.
* **Compression for thin clients.** Byte-shuffled zstd shrinks the binary body 2.2× for ~5 ms of CPU; worthless on a
  5 Gbit/s in-VPC link, worth offering to clients below ~2 Gbit/s.
* **Faster cold start** (parallel prefetch of the 19 GB at the volume's 400 MB/s instead of ~240 MB/s) and a second
  host behind a load balancer for zero-downtime dataset updates.

## 9. Research log

I researched Solvares only as a business (product pages, press releases, marketplace/G-Cloud listings, case studies)
and did not search for, open or read anything describing how Solvares or Stäfn compute distance matrices. No such
material was encountered by accident. Routing literature and open-source engines (OSRM as a reference) were used
freely, as allowed.

Sources: [github.com/KnorpelSenf](https://github.com/KnorpelSenf) ·
[VISITOUR product page](https://solvares-fieldservice.com/en/products/visitour/) ·
[Solvares Field Service — about us](https://solvares-fieldservice.com/en/about-us/) ·
[Solvares Group](https://solvares.com/en/) ·
[Microsoft Dynamics 365 blog, 2026-07-14](https://www.microsoft.com/en-us/dynamics-365/blog/it-professional/2026/07/14/dynamics-365-field-service-scheduling-optimization-solvares-visitour/) ·
[UK G-Cloud listing (AWS hosting, UK data)](https://www.applytosupply.digitalmarketplace.service.gov.uk/g-cloud/services/769562904370406) ·
[HomeServe case study](https://solvares-fieldservice.com/en/news/homeserve-field-scheduling-visitour-success/)
