import argparse
import concurrent.futures
import csv
import json
import math
import random

import numpy as np
import requests
from pyvrp import Model, Route, Solution
from pyvrp.stop import MaxRuntime

HOUR = 3600
SHIFT_START, SHIFT_END = 8 * HOUR, 18 * HOUR
UNREACHABLE = 10 * 24 * HOUR


def read_points(path, count, seed):
    with open(path) as f:
        pool = [(float(lat), float(lon)) for lat, lon in csv.reader(f)]
    return random.Random(seed).sample(pool, count)


def ours(url, points):
    body = {"coordinates": [{"lat": lat, "lon": lon} for lat, lon in points]}
    matrix = requests.post(url, json=body, timeout=60).json()
    as_array = lambda rows: np.array([[UNREACHABLE if v is None else v for v in row] for row in rows], dtype=np.int64)
    return as_array(matrix["times"]), as_array(matrix["distances"])


def osrm(url, points):
    coordinates = ";".join(f"{lon:.7f},{lat:.7f}" for lat, lon in points)
    table = requests.get(f"{url}/table/v1/driving/{coordinates}?annotations=duration,distance", timeout=60).json()
    as_array = lambda rows: np.array([[UNREACHABLE if v is None else round(v) for v in row] for row in rows], dtype=np.int64)
    return as_array(table["durations"]), as_array(table["distances"])


def haversine_m(a, b):
    (lat1, lon1), (lat2, lon2) = [(math.radians(p[0]), math.radians(p[1])) for p in (a, b)]
    h = math.sin((lat2 - lat1) / 2) ** 2 + math.cos(lat1) * math.cos(lat2) * math.sin((lon2 - lon1) / 2) ** 2
    return 2 * 6_371_008.8 * math.asin(math.sqrt(h))


def instance(technicians, jobs, seed):
    rng = random.Random(seed)
    services = [rng.choice([30, 45, 60, 60, 90]) * 60 for _ in range(jobs)]
    windows = [rng.choice([(8 * HOUR, 11 * HOUR), (12 * HOUR, 15 * HOUR), (8 * HOUR, 15 * HOUR)]) for _ in range(jobs)]
    return {"technicians": technicians, "services": services, "windows": windows}


def model(spec, durations, distances):
    m = Model()
    locations = [m.add_location(x=i, y=0) for i in range(len(durations))]
    for home in locations[: spec["technicians"]]:
        depot = m.add_depot(home)
        m.add_vehicle_type(1, start_depot=depot, end_depot=depot, tw_early=SHIFT_START, tw_late=SHIFT_END, unit_distance_cost=0, unit_duration_cost=1)
    for location, service, (early, late) in zip(locations[spec["technicians"] :], spec["services"], spec["windows"]):
        m.add_client(location, service_duration=service, tw_early=early, tw_late=late)
    for i, a in enumerate(locations):
        for j, b in enumerate(locations):
            m.add_edge(a, b, distance=int(distances[i, j]), duration=int(durations[i, j]))
    return m


def plan_and_evaluate(args):
    spec, plan_matrices, truth_matrices, seed, runtime = args
    result = model(spec, *plan_matrices).solve(MaxRuntime(runtime), seed=seed, display=False)
    truth_data = model(spec, *truth_matrices).data()
    routes = [Route(truth_data, [a.idx for a in route if a.is_client()], route.vehicle_type()) for route in result.best.routes()]
    retimed = Solution(truth_data, routes)
    return {
        "planned_feasible": result.best.is_feasible(),
        "travel_h": sum(r.travel_duration() for r in routes) / HOUR,
        "lateness_min": retimed.time_warp() / 60,
        "late_routes": sum(r.has_time_warp() for r in routes),
        "feasible_under_truth": retimed.is_feasible(),
        "routes": len(routes),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--points", required=True)
    parser.add_argument("--ours", required=True)
    parser.add_argument("--osrm", required=True)
    parser.add_argument("--technicians", type=int, default=30)
    parser.add_argument("--jobs", type=int, default=150)
    parser.add_argument("--instances", type=int, default=3)
    parser.add_argument("--seeds", type=int, default=4)
    parser.add_argument("--runtime", type=float, default=20)
    parser.add_argument("--workers", type=int, default=15)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    tasks, labels = [], []
    for index in range(args.instances):
        wanted = args.technicians + args.jobs
        candidates = read_points(args.points, wanted + 20, seed=100 + index)
        routable = lambda m: ((m[0] >= UNREACHABLE).sum(axis=0) + (m[0] >= UNREACHABLE).sum(axis=1)) < len(candidates) // 2
        keep = routable(osrm(args.osrm, candidates)) & routable(ours(args.ours, candidates))
        points = [p for p, k in zip(candidates, keep) if k][:wanted]
        spec = instance(args.technicians, args.jobs, seed=200 + index)
        truth = osrm(args.osrm, points)
        mine = ours(args.ours, points)
        assert (truth[0] < UNREACHABLE).all() and (mine[0] < UNREACHABLE).all(), "instance contains unroutable pairs"
        rng = np.random.default_rng(300 + index)
        crow = np.array([[haversine_m(a, b) for b in points] for a in points])
        reachable = truth[0] < UNREACHABLE
        speed = np.median(crow[reachable & (truth[0] > 0)] / truth[0][reachable & (truth[0] > 0)])
        variants = {
            "osrm (truth)": truth,
            "ours": mine,
            "ours x median bias": ((mine[0] * np.median(truth[0][reachable & (mine[0] > 0)] / mine[0][reachable & (mine[0] > 0)])).round().astype(np.int64), mine[1]),
            "symmetrised truth": (((truth[0] + truth[0].T) / 2).round().astype(np.int64), truth[1]),
            "straight line / median speed": ((crow / speed).round().astype(np.int64), crow.round().astype(np.int64)),
        }
        for sigma in (0.01, 0.02, 0.05, 0.10, 0.20):
            noisy = (truth[0] * np.exp(rng.normal(0, sigma, truth[0].shape))).round().astype(np.int64)
            np.fill_diagonal(noisy, 0)
            variants[f"truth x lognormal noise {int(sigma * 100)}%"] = (noisy, truth[1])
        for label, matrices in variants.items():
            for seed in range(args.seeds):
                tasks.append((spec, matrices, truth, seed, args.runtime))
                labels.append((index, label))
        ratio = mine[0][reachable & (truth[0] > 60)] / truth[0][reachable & (truth[0] > 60)]
        print(f"instance {index}: ours/osrm duration ratio median {np.median(ratio):.3f}, p5 {np.quantile(ratio, 0.05):.3f}, p95 {np.quantile(ratio, 0.95):.3f}", flush=True)

    with concurrent.futures.ProcessPoolExecutor(args.workers) as pool:
        results = list(pool.map(plan_and_evaluate, tasks))

    rows = [{"instance": i, "variant": label, **r} for (i, label), r in zip(labels, results)]
    with open(args.output, "w") as f:
        json.dump(rows, f, indent=1)

    baseline = {i: np.mean([r["travel_h"] for r in rows if r["instance"] == i and r["variant"] == "osrm (truth)"]) for i in range(args.instances)}
    print(f"{'variant':<34} {'travel vs truth-planned':>24} {'seed spread':>12} {'lateness/plan':>14} {'plans on time':>14}")
    for label in dict.fromkeys(label for _, label in labels):
        mine = [r for r in rows if r["variant"] == label]
        deltas = [100 * (r["travel_h"] / baseline[r["instance"]] - 1) for r in mine]
        print(
            f"{label:<34} {np.mean(deltas):>+23.2f}% {np.std(deltas):>11.2f}% {np.mean([r['lateness_min'] for r in mine]):>11.1f} min"
            f" {100 * np.mean([r['feasible_under_truth'] for r in mine]):>13.0f}%"
        )


if __name__ == "__main__":
    main()
