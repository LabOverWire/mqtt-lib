#!/usr/bin/env python3
"""HOL blocking v4 frame packing analysis.

Compares v3 (Greedy) vs v4 (StreamIsolated) results side by side
for per-topic and per-publish strategies.

Usage:
    python hol_v4_frame_packing.py [v3_dir] [v4_dir]

Default dirs:
    v3: experiments/results-v5/02_hol_blocking
    v4: experiments/results-v5/02_hol_blocking
"""

import json
import math
import re
import sys
from collections import defaultdict
from pathlib import Path

from scipy.stats import mannwhitneyu


def load_json(path: Path) -> dict:
    with open(path) as f:
        return json.load(f)


def mean(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0


def std(values: list[float]) -> float:
    if len(values) < 2:
        return 0.0
    m = mean(values)
    return math.sqrt(sum((v - m) ** 2 for v in values) / (len(values) - 1))


def ci95(values: list[float]) -> float:
    if len(values) < 2:
        return 0.0
    return 1.96 * std(values) / math.sqrt(len(values))


def parse_filename(name: str) -> tuple[str, int, int] | None:
    m = re.match(r"(.+?)_loss(\d+)pct_run(\d+)\.json", name)
    if m:
        return m.group(1), int(m.group(2)), int(m.group(3))
    return None


def extract_metrics(data: dict) -> dict:
    results = data["results"]
    return {
        "wcorr": results["windowed_correlation"],
        "detrended_corr": results.get("detrended_correlation"),
        "spread_mean": results.get("inter_topic_spread_mean_us"),
        "spread_p95": results.get("inter_topic_spread_p95_us"),
        "spread_max": results.get("inter_topic_spread_max_us"),
        "spike_iso": results.get("spike_isolation_ratio"),
        "measured_rate": results["measured_rate"],
        "p50_mean": mean([t["p50_us"] for t in results["topics"]]),
    }


def load_results(results_dir: Path, transport_filter: list[str] | None = None) -> dict:
    grouped = defaultdict(list)
    for f in sorted(results_dir.glob("*.json")):
        if "_pub.json" in f.name:
            continue
        parsed = parse_filename(f.name)
        if not parsed:
            continue
        transport, loss_pct, _run = parsed
        if transport_filter and transport not in transport_filter:
            continue
        try:
            data = load_json(f)
            metrics = extract_metrics(data)
            grouped[(transport, loss_pct)].append(metrics)
        except (json.JSONDecodeError, KeyError) as e:
            print(f"  WARN: skipping {f.name}: {e}", file=sys.stderr)
    return grouped


METRIC_NAMES = {
    "spread_mean": "Inter-topic spread (mean, us)",
    "spread_p95": "Inter-topic spread (p95, us)",
    "wcorr": "Windowed correlation",
    "detrended_corr": "Detrended correlation",
    "spike_iso": "Spike isolation ratio",
}


def compare_strategy(strategy: str, v3_key: str, v4_key: str,
                     v3_data: dict, v4_data: dict):
    losses = [0, 1, 2, 5]
    num_comparisons = len(losses) * len(METRIC_NAMES)
    bonferroni_alpha = 0.05 / num_comparisons

    print(f"\n{'=' * 100}")
    print(f"  {strategy.upper()}: Greedy (v3) vs StreamIsolated (v4)")
    print(f"{'=' * 100}")

    for metric_key, metric_label in METRIC_NAMES.items():
        print(f"\n  {metric_label}:")
        print(f"  {'Loss':<6} {'Greedy':<22} {'Isolated':<22} {'Delta':<12} {'p-value':<10} {'Sig?':<5}")
        print(f"  {'-' * 80}")

        for loss in losses:
            v3_vals = [r[metric_key] for r in v3_data.get((v3_key, loss), [])
                       if r[metric_key] is not None]
            v4_vals = [r[metric_key] for r in v4_data.get((v4_key, loss), [])
                       if r[metric_key] is not None]

            if not v3_vals or not v4_vals:
                print(f"  {loss}%     {'N/A':<22} {'N/A':<22}")
                continue

            v3_str = f"{mean(v3_vals):>8.2f} +/- {ci95(v3_vals):>6.2f}"
            v4_str = f"{mean(v4_vals):>8.2f} +/- {ci95(v4_vals):>6.2f}"

            delta = mean(v4_vals) - mean(v3_vals)
            pct = (delta / mean(v3_vals) * 100) if mean(v3_vals) != 0 else 0
            delta_str = f"{pct:>+6.1f}%"

            try:
                _, p_val = mannwhitneyu(v3_vals, v4_vals, alternative="two-sided")
                sig = "*" if p_val < bonferroni_alpha else ""
                p_str = f"{p_val:.4f}"
            except ValueError:
                p_str = "N/A"
                sig = ""

            print(f"  {loss}%     {v3_str:<22} {v4_str:<22} {delta_str:<12} {p_str:<10} {sig:<5}")


def print_summary_table(v3_data: dict, v4_data: dict):
    losses = [0, 1, 2, 5]
    strategies = [
        ("per-topic", "quic-pertopic", "quic-pertopic-isolated"),
        ("per-publish", "quic-perpub", "quic-perpub-isolated"),
    ]

    print(f"\n{'=' * 100}")
    print("SUMMARY: Greedy vs StreamIsolated — inter-topic spread mean (us)")
    print(f"{'=' * 100}")
    print(f"\n{'Strategy':<15} {'Packing':<16} ", end="")
    for loss in losses:
        print(f"{'loss ' + str(loss) + '%':<18} ", end="")
    print()
    print("-" * 100)

    for label, v3_key, v4_key in strategies:
        for packing, data, key in [("Greedy", v3_data, v3_key),
                                    ("StreamIsolated", v4_data, v4_key)]:
            print(f"{label:<15} {packing:<16} ", end="")
            for loss in losses:
                vals = [r["spread_mean"] for r in data.get((key, loss), [])
                        if r["spread_mean"] is not None]
                if vals:
                    print(f"{mean(vals):>8.1f} +/- {ci95(vals):>5.1f}  ", end="")
                else:
                    print(f"{'N/A':<18} ", end="")
            print()
        print()


def main():
    base = Path(__file__).parent.parent

    if len(sys.argv) > 2:
        v3_dir = Path(sys.argv[1])
        v4_dir = Path(sys.argv[2])
    else:
        v3_dir = base / "results-v5" / "02_hol_blocking"
        v4_dir = base / "results-v5" / "02_hol_blocking"

    for d, label in [(v3_dir, "v3"), (v4_dir, "v4")]:
        if not d.exists():
            print(f"{label} results directory not found: {d}", file=sys.stderr)
            sys.exit(1)

    v3_json = len(list(v3_dir.glob("*.json")))
    v4_json = len(list(v4_dir.glob("*.json")))
    print(f"v3 (Greedy):         {v3_dir} ({v3_json} JSON files)")
    print(f"v4 (StreamIsolated): {v4_dir} ({v4_json} JSON files)")

    v3_data = load_results(v3_dir, ["quic-pertopic", "quic-perpub"])
    v4_data = load_results(v4_dir, ["quic-pertopic-isolated", "quic-perpub-isolated"])

    compare_strategy("per-topic", "quic-pertopic", "quic-pertopic-isolated",
                     v3_data, v4_data)
    compare_strategy("per-publish", "quic-perpub", "quic-perpub-isolated",
                     v3_data, v4_data)

    print_summary_table(v3_data, v4_data)


if __name__ == "__main__":
    main()
