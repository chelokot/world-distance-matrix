#!/usr/bin/env python3
import argparse
import http.client
import io
import json
import math
import random
import statistics
import struct
import time
import urllib.parse

ACCEPT = {"binary": "application/vnd.distance-matrix.v1", "compact": "application/vnd.distance-matrix.compact.v1", "json": "application/json"}
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
    connection.request("POST", "/matrix", payload, {"Content-Type": content_type, "Accept": ACCEPT[format], "Accept-Encoding": "zstd" if format == "compact" else "identity"})
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


def decode_compact(stream: io.BytesIO):
    import numpy as np

    _, rows, cols, frames = struct.unpack("<4sIII", stream.read(16))
    row_order = np.frombuffer(stream.read(4 * rows), "<u4")
    col_order = np.frombuffer(stream.read(4 * cols), "<u4")
    encoded = np.empty((2, rows, cols), np.int64)
    no_route = np.empty((rows, cols), bool)
    previous = np.zeros((2, 1, cols), np.int64)
    first = 0
    for _ in range(frames):
        (height,) = struct.unpack("<I", stream.read(4))
        cells = height * cols
        payload = stream.read(8 * cells + (cells + 7) // 8)
        planes = np.frombuffer(payload, np.uint8, 8 * cells).reshape(2, 4, cells)
        zigzag = np.ascontiguousarray(planes.transpose(0, 2, 1)).view("<u4").reshape(2, height, cols).astype(np.int64)
        block = previous + np.cumsum(np.cumsum((zigzag >> 1) ^ -(zigzag & 1), axis=2), axis=1)
        previous = block[:, -1:, :]
        encoded[:, first : first + height] = block
        no_route[first : first + height] = np.unpackbits(np.frombuffer(payload, np.uint8, offset=8 * cells), count=cells, bitorder="little").reshape(height, cols)
        first += height
    queue_us, prepare_us, compute_us = struct.unpack("<III", stream.read(12))
    encoded[:, no_route] = NO_ROUTE
    matrix = np.empty((2, rows, cols), np.uint32)
    matrix[:, row_order[:, None], col_order[None, :]] = encoded
    return matrix[0], matrix[1], (queue_us / 1000, prepare_us / 1000, compute_us / 1000)


def fetch(connection: http.client.HTTPConnection, payload: bytes, format: str) -> tuple[float, int, int, str]:
    started = time.perf_counter()
    response = post(connection, payload, format)
    server = response.getheader("server-timing", "")
    if format == "compact":
        from compression import zstd

        data = response.read()
        size = len(data)
        body = zstd.decompress(data) if response.getheader("content-encoding") == "zstd" else data
        distances, _, (queue, prepare, compute) = decode_compact(io.BytesIO(body))
        unreachable = int((distances == NO_ROUTE).sum())
        server = f"queue {queue:.1f} ms, snapping and setup {prepare:.1f} ms, all rows {compute:.1f} ms"
    elif format == "binary":
        data = response.read()
        size = len(data)
        unreachable = memoryview(data)[16:].cast("I").tolist().count(NO_ROUTE) // 2
    else:
        data = response.read()
        size = len(data)
        unreachable = sum(row.count(None) for row in json.loads(data)["times"])
    return (time.perf_counter() - started) * 1000, size, unreachable, server


def main() -> None:
    parser = argparse.ArgumentParser(description="Request square distance matrices and report client-side time to last byte, decoding included.")
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
        fetch(connection, payload, args.format)
    results = [fetch(connection, payload, args.format) for payload in payloads[3:]]
    latencies = sorted(result[0] for result in results)
    percentile = lambda q: latencies[min(len(latencies) - 1, round(q * (len(latencies) - 1)))]
    print(f"{args.points} x {args.points} {args.format}, {len(results)} requests on one connection, response {results[-1][1] / 1e6:.2f} MB")
    print(f"time to last byte incl. decoding: p50 {percentile(0.5):.1f} ms, p90 {percentile(0.9):.1f} ms, p99 {percentile(0.99):.1f} ms, max {latencies[-1]:.1f} ms")
    print(f"mean {statistics.fmean(latencies):.1f} ms; cells without a route in the last response: {results[-1][2]:,} of {args.points ** 2:,}")
    print(f"server, last response: {results[-1][3]}")


if __name__ == "__main__":
    main()
