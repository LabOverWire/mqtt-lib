#!/usr/bin/env python3
"""Aggregate MQoQ benchmark JSON results into CSV summary tables."""

import json
import math
import sys
from pathlib import Path


def load_results(results_dir: Path) -> dict[str, list]:
    experiments: dict[str, list] = {}
    for exp_dir in sorted(results_dir.iterdir()):
        if not exp_dir.is_dir() or exp_dir.name.startswith("."):
            continue
        runs = []
        for json_file in sorted(exp_dir.glob("*.json")):
            with open(json_file) as f:
                try:
                    runs.append({"file": json_file.name, "data": json.load(f)})
                except json.JSONDecodeError:
                    print(f"warning: skipping invalid JSON: {json_file}", file=sys.stderr)
        if runs:
            experiments[exp_dir.name] = runs
    return experiments


def group_by_label(runs: list) -> dict[str, list]:
    groups: dict[str, list] = {}
    for run in runs:
        name = run["file"].rsplit("_run", 1)[0]
        groups.setdefault(name, []).append(run["data"])
    return groups


def mean(values: list[float]) -> float:
    if not values:
        return 0.0
    return sum(values) / len(values)


def stdev(values: list[float]) -> float:
    if len(values) < 2:
        return 0.0
    avg = mean(values)
    variance = sum((v - avg) ** 2 for v in values) / (len(values) - 1)
    return math.sqrt(variance)


def ci95(values: list[float]) -> float:
    if len(values) < 2:
        return 0.0
    return 1.96 * stdev(values) / math.sqrt(len(values))


def extract_metric(data: dict) -> dict[str, float]:
    results = data.get("results", {})
    mode = data.get("mode", "")

    if mode == "throughput":
        return {"throughput_avg": results.get("throughput_avg", 0)}
    elif mode == "latency":
        return {
            "p50_us": results.get("p50_us", 0),
            "p95_us": results.get("p95_us", 0),
            "p99_us": results.get("p99_us", 0),
        }
    elif mode == "connections":
        return {
            "connections_per_sec": results.get("connections_per_sec", 0),
            "p50_connect_us": results.get("p50_connect_us", 0),
        }
    elif mode == "hol-blocking":
        return {"correlation": results.get("correlation", 0)}
    return {}


def aggregate_experiment(runs_by_label: dict[str, list]) -> list[dict]:
    rows = []
    for label, runs in sorted(runs_by_label.items()):
        metrics: dict[str, list[float]] = {}
        for run in runs:
            for key, val in extract_metric(run).items():
                metrics.setdefault(key, []).append(float(val))

        row = {"label": label, "n": len(runs)}
        for key, values in sorted(metrics.items()):
            row[f"{key}_mean"] = f"{mean(values):.2f}"
            row[f"{key}_stdev"] = f"{stdev(values):.2f}"
            row[f"{key}_ci95"] = f"{ci95(values):.2f}"
        config = runs[0].get("config", {}) if runs else {}
        row["transport"] = config.get("transport", "")
        row["quic_strategy"] = config.get("quic_stream_strategy", "")
        rows.append(row)
    return rows


def write_csv(rows: list[dict], output: Path):
    if not rows:
        return
    all_keys = []
    for row in rows:
        for k in row:
            if k not in all_keys:
                all_keys.append(k)

    with open(output, "w") as f:
        f.write(",".join(all_keys) + "\n")
        for row in rows:
            f.write(",".join(str(row.get(k, "")) for k in all_keys) + "\n")


def main():
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent.parent / "results-phase1"
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else results_dir

    if not results_dir.exists():
        print(f"results directory not found: {results_dir}", file=sys.stderr)
        sys.exit(1)

    experiments = load_results(results_dir)
    for exp_name, runs in experiments.items():
        grouped = group_by_label(runs)
        rows = aggregate_experiment(grouped)
        output_file = output_dir / f"{exp_name}_summary.csv"
        write_csv(rows, output_file)
        print(f"wrote {output_file} ({len(rows)} rows)")


if __name__ == "__main__":
    main()
