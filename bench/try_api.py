#!/usr/bin/env python3
import argparse
import http.client
import json
import math
import random
import statistics
import time
import urllib.parse

BINARY = "application/vnd.distance-matrix.v1"
NO_ROUTE = 0xFFFFFFFF


def random_points(center: tuple[float, float], radius_km: float, count: int, rng: random.Random) -> list[dict[str, float]]:
    lat, lon = center
    points = []
    for _ in range(count):
        r = radius_km * math.sqrt(rng.random())
        angle = rng.uniform(0, 2 * math.pi)
        points.append({"lat": lat + r * math.cos(angle) / 111.32, "lon": lon + r * math.sin(angle) / (111.32 * math.cos(math.radians(lat)))})
    return points


def fetch(connection: http.client.HTTPConnection, body: bytes, binary: bool) -> tuple[float, bytes]:
    started = time.perf_counter()
    connection.request("POST", "/matrix", body, {"Content-Type": "application/json", "Accept": BINARY if binary else "application/json"})
    response = connection.getresponse()
    payload = response.read()
    if response.status != 200:
        raise SystemExit(f"HTTP {response.status}: {payload[:300].decode(errors='replace')}")
    if binary:
        memoryview(payload)[16:].cast("I")
    else:
        json.loads(payload)
    return (time.perf_counter() - started) * 1000, payload


def cells_without_route(payload: bytes, binary: bool) -> int:
    if binary:
        return memoryview(payload)[16:].cast("I").tolist().count(NO_ROUTE) // 2
    return sum(row.count(None) for row in json.loads(payload)["times"])


def main() -> None:
    parser = argparse.ArgumentParser(description="Request square distance matrices and report client-side time-to-last-byte.")
    parser.add_argument("--url", default="http://100.57.61.188:8080")
    parser.add_argument("--center", default="53.55,10.0", help="lat,lon around which random points are drawn (default: Hamburg)")
    parser.add_argument("--radius-km", type=float, default=25.0)
    parser.add_argument("--points", type=int, default=1000)
    parser.add_argument("--requests", type=int, default=30)
    parser.add_argument("--format", choices=["binary", "json"], default="binary")
    args = parser.parse_args()
    url = urllib.parse.urlsplit(args.url)
    center = tuple(float(part) for part in args.center.split(","))
    binary = args.format == "binary"
    rng = random.Random(1)
    connection = http.client.HTTPConnection(url.hostname, url.port or 80, timeout=60)
    bodies = [json.dumps({"coordinates": random_points(center, args.radius_km, args.points, rng)}).encode() for _ in range(args.requests + 3)]
    for body in bodies[:3]:
        fetch(connection, body, binary)
    results = [fetch(connection, body, binary) for body in bodies[3:]]
    latencies = sorted(elapsed for elapsed, _ in results)
    percentile = lambda q: latencies[min(len(latencies) - 1, round(q * (len(latencies) - 1)))]
    last = results[-1][1]
    print(f"{args.points} x {args.points} {args.format}, {len(results)} requests, response {len(last) / 1e6:.1f} MB")
    print(f"time to last byte incl. decoding: p50 {percentile(0.5):.1f} ms, p90 {percentile(0.9):.1f} ms, p99 {percentile(0.99):.1f} ms, max {latencies[-1]:.1f} ms")
    print(f"mean {statistics.fmean(latencies):.1f} ms; cells without a route in the last response: {cells_without_route(last, binary)}")


if __name__ == "__main__":
    main()
