#!/usr/bin/env python3
"""HOL blocking v3 analysis.

Aggregates v3 JSON results, computes summary statistics,
and validates that no queue buildup occurred.

Usage:
    python hol_v3_analysis.py [results_dir]

Default results_dir: experiments/results_v3/02_hol_blocking
"""

import json
import math
import re
import sys
from collections import defaultdict
from pathlib import Path


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
    topics = results["topics"]

    p50s = [t["p50_us"] for t in topics]

    return {
        "wcorr": results["windowed_correlation"],
        "detrended_corr": results.get("detrended_correlation", None),
        "raw_corr": results["raw_correlation"],
        "spread_mean": results.get("inter_topic_spread_mean_us", None),
        "spread_p95": results.get("inter_topic_spread_p95_us", None),
        "spread_max": results.get("inter_topic_spread_max_us", None),
        "spike_iso": results.get("spike_isolation_ratio", None),
        "cluster_ratio": results.get("inter_arrival_cluster_ratio", None),
        "total_msgs": results["total_messages"],
        "measured_rate": results["measured_rate"],
        "p50_mean": mean(p50s),
        "p50_max": max(p50s),
        "p50_cv": std(p50s) / mean(p50s) if mean(p50s) > 0 else 0,
    }


def aggregate_by_condition(results_dir: Path) -> dict:
    grouped = defaultdict(list)

    for f in sorted(results_dir.glob("*.json")):
        if "_pub.json" in f.name:
            continue
        parsed = parse_filename(f.name)
        if not parsed:
            continue
        transport, loss_pct, _run = parsed
        try:
            data = load_json(f)
            metrics = extract_metrics(data)
            grouped[(transport, loss_pct)].append(metrics)
        except (json.JSONDecodeError, KeyError) as e:
            print(f"  WARN: skipping {f.name}: {e}", file=sys.stderr)

    return grouped


def print_summary(grouped: dict):
    transports_order = ["tcp", "quic-control", "quic-pertopic", "quic-perpub"]
    losses_order = [0, 1, 2, 5]

    print("\n" + "=" * 110)
    print("HOL BLOCKING v3 — SUMMARY")
    print("=" * 110)

    print(f"\n{'Transport':<18} {'Loss':<6} {'Runs':<5} "
          f"{'Spread mean':<14} {'Spread p95':<14} "
          f"{'Detrend corr':<14} {'Wcorr':<14} "
          f"{'Rate msg/s':<12} {'p50 us':<10}")
    print("-" * 110)

    for transport in transports_order:
        for loss in losses_order:
            key = (transport, loss)
            if key not in grouped:
                continue
            runs = grouped[key]
            n = len(runs)

            spread_means = [r["spread_mean"] for r in runs if r["spread_mean"] is not None]
            spread_p95s = [r["spread_p95"] for r in runs if r["spread_p95"] is not None]
            detrended = [r["detrended_corr"] for r in runs if r["detrended_corr"] is not None]
            wcorrs = [r["wcorr"] for r in runs]
            rates = [r["measured_rate"] for r in runs]
            p50s = [r["p50_mean"] for r in runs]

            sm = f"{mean(spread_means):7.1f}±{ci95(spread_means):5.1f}" if spread_means else "N/A"
            sp = f"{mean(spread_p95s):7.1f}±{ci95(spread_p95s):5.1f}" if spread_p95s else "N/A"
            dc = f"{mean(detrended):7.4f}±{ci95(detrended):5.4f}" if detrended else "N/A"
            wc = f"{mean(wcorrs):7.4f}±{ci95(wcorrs):5.4f}"
            rt = f"{mean(rates):7.0f}"
            p5 = f"{mean(p50s):7.0f}"

            print(f"{transport:<18} {loss:<6} {n:<5} {sm:<14} {sp:<14} {dc:<14} {wc:<14} {rt:<12} {p5:<10}")

    print()


def check_queue_buildup(grouped: dict):
    print("\n" + "=" * 80)
    print("QUEUE BUILDUP CHECK")
    print("=" * 80)
    print("If p50 latency >> baseline RTT (expected ~50000us at 25ms delay),")
    print("queue buildup is occurring and the rate needs to be lowered.\n")

    baseline_p50 = {}
    for (transport, loss), runs in grouped.items():
        if loss == 0:
            baseline_p50[transport] = mean([r["p50_mean"] for r in runs])

    issues = []
    for (transport, loss), runs in sorted(grouped.items()):
        p50 = mean([r["p50_mean"] for r in runs])
        base = baseline_p50.get(transport, p50)
        ratio = p50 / base if base > 0 else 0

        status = "OK" if ratio < 2.0 else "WARN"
        if ratio >= 5.0:
            status = "QUEUE BUILDUP"
            issues.append(f"{transport} at {loss}% loss: p50={p50:.0f}us ({ratio:.1f}x baseline)")

        print(f"  {transport:<18} loss={loss}%: p50={p50:>10.0f}us  "
              f"({ratio:>5.1f}x baseline)  [{status}]")

    if issues:
        print(f"\n  ISSUES FOUND ({len(issues)}):")
        for issue in issues:
            print(f"    - {issue}")
        print("  Consider reducing --rate below 500 msg/s")
    else:
        print("\n  No queue buildup detected.")


def print_spike_isolation(grouped: dict):
    transports_order = ["tcp", "quic-control", "quic-pertopic", "quic-perpub"]
    losses_order = [0, 1, 2, 5]

    print("\n" + "=" * 80)
    print("SPIKE ISOLATION RATIO (from trace data)")
    print("=" * 80)
    print("1.0 = all spikes co-occur (HOL blocking), 0.0 = fully isolated\n")

    for transport in transports_order:
        for loss in losses_order:
            key = (transport, loss)
            if key not in grouped:
                continue
            runs = grouped[key]
            spike_vals = [r["spike_iso"] for r in runs if r["spike_iso"] is not None]
            if not spike_vals:
                continue
            m = mean(spike_vals)
            c = ci95(spike_vals)
            print(f"  {transport:<18} loss={loss}%: {m:.4f} ± {c:.4f}  (n={len(spike_vals)})")


def print_latex_table(grouped: dict):
    transports_order = ["tcp", "quic-control", "quic-pertopic", "quic-perpub"]
    labels = {
        "tcp": "TCP",
        "quic-control": "QUIC control-only",
        "quic-pertopic": "QUIC per-topic",
        "quic-perpub": "QUIC per-publish",
    }
    losses_order = [0, 1, 2, 5]

    print("\n" + "=" * 80)
    print("LATEX TABLE (inter-topic spread mean, us)")
    print("=" * 80)
    print(r"\begin{tabular}{l" + "r" * len(losses_order) + "}")
    print(r"\toprule")
    header = "Transport & " + " & ".join(f"{l}\\% loss" for l in losses_order) + r" \\"
    print(header)
    print(r"\midrule")

    for transport in transports_order:
        row = labels.get(transport, transport)
        for loss in losses_order:
            key = (transport, loss)
            if key not in grouped:
                row += " & ---"
                continue
            runs = grouped[key]
            vals = [r["spread_mean"] for r in runs if r["spread_mean"] is not None]
            if vals:
                row += f" & {mean(vals):.1f}"
            else:
                row += " & ---"
        row += r" \\"
        print(row)

    print(r"\bottomrule")
    print(r"\end{tabular}")


def main():
    if len(sys.argv) > 1:
        results_dir = Path(sys.argv[1])
    else:
        results_dir = Path(__file__).parent.parent / "results_v3" / "02_hol_blocking"

    if not results_dir.exists():
        print(f"Results directory not found: {results_dir}", file=sys.stderr)
        sys.exit(1)

    json_count = len(list(results_dir.glob("*.json")))
    print(f"Loading from: {results_dir}")
    print(f"JSON files found: {json_count}")

    grouped = aggregate_by_condition(results_dir)
    if not grouped:
        print("No valid results found.", file=sys.stderr)
        sys.exit(1)

    print_summary(grouped)
    check_queue_buildup(grouped)
    print_spike_isolation(grouped)
    print_latex_table(grouped)


if __name__ == "__main__":
    main()
