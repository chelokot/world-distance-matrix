#!/usr/bin/env python3
import argparse
import http.client
import random
import struct
import urllib.parse

import numpy as np

from try_api import NO_ROUTE, body, on_globe, post

REGIONS = {"world": None, "europe": (36.0, 70.0, -10.0, 40.0)}


def matrix(connection: http.client.HTTPConnection, points: list[tuple[float, float]]) -> tuple[np.ndarray, np.ndarray]:
    data = post(connection, body(points, True), "binary").read()
    rows, cols = struct.unpack_from("<II", data, 8)
    words = np.frombuffer(data, "<u4", offset=16).reshape(rows, 2, cols)
    return words[:, 0].astype(np.int64), words[:, 1].astype(np.int64)


def near_roads(connection: http.client.HTTPConnection, count: int, rng: random.Random, box) -> list[tuple[float, float]]:
    found = []
    while len(found) < count:
        batch = on_globe(250, rng) if box is None else [(rng.uniform(box[0], box[1]), rng.uniform(box[2], box[3])) for _ in range(250)]
        pairs = [point for lat, lon in batch for point in ((lat, lon), (lat + 1e-6, lon))]
        data = post(connection, body(pairs, True, list(range(0, 500, 2)), list(range(1, 500, 2))), "binary").read()
        distances = memoryview(data)[16:].cast("I")
        found += [point for k, point in enumerate(batch) if distances[k * 500 + k] != NO_ROUTE]
    return found[:count]


def great_circle_m(points: np.ndarray) -> np.ndarray:
    lat, lon = np.radians(points[:, 0]), np.radians(points[:, 1])
    h = np.sin((lat[:, None] - lat[None, :]) / 2) ** 2 + np.cos(lat[:, None]) * np.cos(lat[None, :]) * np.sin((lon[:, None] - lon[None, :]) / 2) ** 2
    return 2 * 6371008.8 * np.arcsin(np.sqrt(np.minimum(h, 1)))


def main() -> None:
    parser = argparse.ArgumentParser(description="Sample road-side points, request their matrix and check it for impossible or suspicious cells.")
    parser.add_argument("--url", default="http://3.65.232.220:8080")
    parser.add_argument("--region", choices=list(REGIONS), default="europe")
    parser.add_argument("--points", type=int, default=1500)
    parser.add_argument("--show", type=int, default=5, help="worst pairs to print per finding")
    args = parser.parse_args()
    url = urllib.parse.urlsplit(args.url)
    connection = http.client.HTTPConnection(url.hostname, url.port or 80, timeout=600)
    rng = random.Random(42)
    points = near_roads(connection, args.points, rng, REGIONS[args.region])
    coords = np.array(points)
    distances, times = matrix(connection, points)
    straight = great_circle_m(coords)
    count = len(points)
    routed = (distances != NO_ROUTE) & ~np.eye(count, dtype=bool)
    speed = np.where(routed & (times > 0), distances / np.maximum(times, 1) * 3.6, 0)
    detour = np.where(routed & (straight > 10_000), distances / np.maximum(straight, 1), 0)
    both_long = routed & routed.T & (times > 600) & (times.T > 600)
    asymmetry = np.where(both_long, np.abs(times - times.T) / np.maximum(np.minimum(times, times.T), 1), 0)
    triples = [(rng.randrange(count), rng.randrange(count), rng.randrange(count)) for _ in range(200_000)]
    violations = sum(1 for a, b, c in triples if routed[a, b] and routed[b, c] and routed[a, c] and times[a, c] > times[a, b] + times[b, c] + 1)
    print(f"{args.region}: {count} road-side points, {routed.sum():,} routed cells of {count * (count - 1):,}")
    print(f"  impossible: shorter than the great circle {(routed & (distances < straight * 0.999 - 2)).sum()}, "
          f"triangle violations in 200k triples {violations}, cells reachable one way only {((distances != NO_ROUTE) != (distances.T != NO_ROUTE)).sum()}")
    findings = {
        "average speed above 130 km/h over 10 km": (routed & (distances > 10_000) & (speed > 130), speed),
        "detour above 5x the great circle": (detour > 5, detour),
        "one direction more than twice the other (both over 10 min)": (asymmetry > 1, asymmetry),
    }
    for label, (mask, score) in findings.items():
        print(f"  {label}: {mask.sum()} cells")
        for i, j in sorted(np.argwhere(mask), key=lambda ij: -score[ij[0], ij[1]])[: args.show]:
            a, b = points[i], points[j]
            print(f"    ({a[0]:.4f},{a[1]:.4f}) -> ({b[0]:.4f},{b[1]:.4f}): {distances[i, j] / 1000:.1f} km, {times[i, j] / 60:.1f} min, "
                  f"back {times[j, i] / 60:.1f} min, great circle {straight[i, j] / 1000:.1f} km")


if __name__ == "__main__":
    main()
