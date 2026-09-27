#!/usr/bin/env python3
import argparse
import ctypes
import functools
import http.client
import json
import math
import pathlib
import random
import statistics
import struct
import time
import urllib.parse

ACCEPT = {"binary": "application/vnd.distance-matrix.v1", "compact": "application/vnd.distance-matrix.compact.v2", "json": "application/json"}
REQUEST = "application/vnd.distance-matrix.request.v1"
NO_ROUTE = 0xFFFFFFFF


def around(center: tuple[float, float], radius_km: float, count: int, rng: random.Random) -> list[tuple[float, float]]:
    lat, lon = center
    points = []
    for _ in range(count):
        r = radius_km * math.sqrt(rng.random())
        angle = rng.uniform(0, 2 * math.pi)
        points.append((lat + r * math.cos(angle) / 111.32, lon + r * math.sin(angle) / (111.32 * math.cos(math.radians(lat)))))
    return points


def on_globe(count: int, rng: random.Random) -> list[tuple[float, float]]:
    return [(math.degrees(math.asin(2 * rng.random() - 1)), 360 * rng.random() - 180) for _ in range(count)]


def body(points: list[tuple[float, float]], binary: bool, sources: list[int] = (), destinations: list[int] = ()) -> bytes:
    if not binary:
        indices = {name: list(values) for name, values in (("sources", sources), ("destinations", destinations)) if values}
        return json.dumps({"coordinates": [{"lat": lat, "lon": lon} for lat, lon in points], **indices}).encode()
    coordinates = [round(value * 1e7) for point in points for value in point]
    return b"DMQ1" + struct.pack(f"<3I{len(coordinates)}i{len(sources) + len(destinations)}I", len(points), len(sources), len(destinations), *coordinates, *sources, *destinations)


def post(connection: http.client.HTTPConnection, payload: bytes, format: str) -> http.client.HTTPResponse:
    content_type = REQUEST if payload[:4] == b"DMQ1" else "application/json"
    connection.request("POST", "/matrix", payload, {"Content-Type": content_type, "Accept": ACCEPT[format]})
    response = connection.getresponse()
    if response.status != 200:
        raise SystemExit(f"HTTP {response.status}: {response.read()[:300].decode(errors='replace')}")
    return response


def near_roads(connection: http.client.HTTPConnection, count: int, rng: random.Random) -> tuple[list[tuple[float, float]], int]:
    found, tried = [], 0
    while len(found) < count:
        batch = on_globe(250, rng)
        tried += len(batch)
        pairs = [point for lat, lon in batch for point in ((lat, lon), (lat + 1e-6, lon))]
        data = post(connection, body(pairs, True, list(range(0, 500, 2)), list(range(1, 500, 2))), "binary").read()
        distances = memoryview(data)[16:].cast("I")
        found += [point for k, point in enumerate(batch) if distances[k * 500 + k] != NO_ROUTE]
    return found[:count], tried


@functools.cache
def compact_decoder() -> ctypes.CDLL:
    library = ctypes.CDLL(str(pathlib.Path(__file__).resolve().parents[1] / "target/release/libdm_web.so"))
    library.input.argtypes = [ctypes.c_size_t]
    for name in ("input", "distances", "server_times"):
        getattr(library, name).restype = ctypes.c_void_p
    return library


def stream_compact(response: http.client.HTTPResponse, cells: int) -> tuple[int, list[int], list[int]]:
    decoder, size = compact_decoder(), 0
    decoder.stream_start(1)
    while not decoder.stream_complete():
        chunk = response.read1(1 << 16)
        if not chunk:
            raise SystemExit("the compact answer ended early")
        size += len(chunk)
        ctypes.memmove(decoder.input(len(chunk)), chunk, len(chunk))
        if not decoder.stream_feed():
            raise SystemExit("the compact answer could not be decoded")
    response.read()
    decoder.stream_finish()
    return size, (ctypes.c_uint32 * cells).from_address(decoder.distances()), (ctypes.c_uint32 * 3).from_address(decoder.server_times())


def fetch(connection: http.client.HTTPConnection, payload: bytes, format: str, points: int) -> tuple[float, int, int, str]:
    started = time.perf_counter()
    response = post(connection, payload, format)
    server = response.getheader("server-timing", "")
    if format == "compact":
        size, distances, (queue, prepare, compute) = stream_compact(response, points * points)
        elapsed = time.perf_counter() - started
        unreachable = list(distances).count(NO_ROUTE)
        server = f"queue {queue / 1000:.1f} ms, snapping and setup {prepare / 1000:.1f} ms, all rows {compute / 1000:.1f} ms"
    elif format == "binary":
        data = response.read()
        elapsed, size = time.perf_counter() - started, len(data)
        unreachable = memoryview(data)[16:].cast("I").tolist().count(NO_ROUTE) // 2
    else:
        data = response.read()
        times = json.loads(data)["times"]
        elapsed, size = time.perf_counter() - started, len(data)
        unreachable = sum(row.count(None) for row in times)
    return elapsed * 1000, size, unreachable, server


def main() -> None:
    parser = argparse.ArgumentParser(description="Request square distance matrices and report client-side time to last byte, decoding included (compact needs cargo build --release -p dm-web).")
    parser.add_argument("--url", default="http://3.65.232.220:8080")
    parser.add_argument("--center", default="53.55,10.0", help="lat,lon around which random points are drawn (default: Hamburg)")
    parser.add_argument("--radius-km", type=float, default=25.0)
    parser.add_argument("--world", action="store_true", help="draw points uniformly over the whole globe instead")
    parser.add_argument("--on-roads", action="store_true", help="with --world, draw from a pool of points within 5 km of a road")
    parser.add_argument("--points", type=int, default=1000)
    parser.add_argument("--requests", type=int, default=30)
    parser.add_argument("--format", choices=list(ACCEPT), default="binary")
    args = parser.parse_args()
    url = urllib.parse.urlsplit(args.url)
    connection = http.client.HTTPConnection(url.hostname, url.port or 80, timeout=120)
    rng = random.Random(1)
    if args.world and args.on_roads:
        pool, tried = near_roads(connection, 2 * args.points, rng)
        print(f"sampled the globe uniformly: {100 * len(pool) / tried:.0f}% of points lie within 5 km of a road")
        batches = [rng.sample(pool, args.points) for _ in range(args.requests + 3)]
    elif args.world:
        batches = [on_globe(args.points, rng) for _ in range(args.requests + 3)]
    else:
        center = tuple(float(part) for part in args.center.split(","))
        batches = [around(center, args.radius_km, args.points, rng) for _ in range(args.requests + 3)]
    payloads = [body(points, args.format != "json") for points in batches]
    for payload in payloads[:3]:
        fetch(connection, payload, args.format, args.points)
    results = [fetch(connection, payload, args.format, args.points) for payload in payloads[3:]]
    latencies = sorted(result[0] for result in results)
    percentile = lambda q: latencies[min(len(latencies) - 1, round(q * (len(latencies) - 1)))]
    print(f"{args.points} x {args.points} {args.format}, {len(results)} requests on one connection, response {results[-1][1] / 1e6:.2f} MB")
    print(f"time to last byte incl. decoding: p50 {percentile(0.5):.1f} ms, p90 {percentile(0.9):.1f} ms, p99 {percentile(0.99):.1f} ms, max {latencies[-1]:.1f} ms")
    print(f"mean {statistics.fmean(latencies):.1f} ms; cells without a route in the last response: {results[-1][2]:,} of {args.points ** 2:,}")
    print(f"server, last response: {results[-1][3]}")


if __name__ == "__main__":
    main()
