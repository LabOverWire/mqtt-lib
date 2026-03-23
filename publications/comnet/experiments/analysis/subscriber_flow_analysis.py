#!/usr/bin/env python3
"""Subscriber-side flow delivery analysis.

Compares control-stream vs per-topic-flow delivery for HOL blocking
and throughput from the subscriber's perspective.

Usage:
    python subscriber_flow_analysis.py [results_dir]

Default results_dir: experiments/results-v5/07_subscriber_flow_delivery
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
    p95s = [t["p95_us"] for t in topics]

    return {
        "wcorr": results["windowed_correlation"],
        "detrended_corr": results.get("detrended_correlation", None),
        "spread_mean": results.get("inter_topic_spread_mean_us", None),
        "spread_p95": results.get("inter_topic_spread_p95_us", None),
        "spread_max": results.get("inter_topic_spread_max_us", None),
        "spike_iso": results.get("spike_isolation_ratio", None),
        "total_msgs": results["total_messages"],
        "measured_rate": results["measured_rate"],
        "p50_mean": mean(p50s),
        "p95_mean": mean(p95s),
        "p50_max": max(p50s),
    }


def aggregate_by_condition(results_dir: Path) -> dict:
    grouped = defaultdict(list)

    for f in sorted(results_dir.glob("*.json")):
        parsed = parse_filename(f.name)
        if not parsed:
            continue
        mode, loss_pct, _run = parsed
        try:
            data = load_json(f)
            metrics = extract_metrics(data)
            grouped[(mode, loss_pct)].append(metrics)
        except (json.JSONDecodeError, KeyError) as e:
            print(f"  WARN: skipping {f.name}: {e}", file=sys.stderr)

    return grouped


def print_summary(grouped: dict):
    modes_order = ["control", "per-topic-flow"]
    losses_order = [0, 1, 2, 5]

    print("\n" + "=" * 120)
    print("SUBSCRIBER FLOW DELIVERY — SUMMARY")
    print("Publisher: TCP | Subscriber: QUIC | Varying: delivery path")
    print("=" * 120)

    print(f"\n{'Mode':<18} {'Loss':<6} {'Runs':<5} "
          f"{'Spread mean':<14} {'Spread p95':<14} "
          f"{'Spike iso':<12} {'Detrend corr':<14} "
          f"{'Rate msg/s':<12} {'p50 us':<10} {'p95 us':<10}")
    print("-" * 120)

    for mode in modes_order:
        for loss in losses_order:
            key = (mode, loss)
            if key not in grouped:
                continue
            runs = grouped[key]
            n = len(runs)

            spread_means = [r["spread_mean"] for r in runs if r["spread_mean"] is not None]
            spread_p95s = [r["spread_p95"] for r in runs if r["spread_p95"] is not None]
            detrended = [r["detrended_corr"] for r in runs if r["detrended_corr"] is not None]
            spike_isos = [r["spike_iso"] for r in runs if r["spike_iso"] is not None]
            rates = [r["measured_rate"] for r in runs]
            p50s = [r["p50_mean"] for r in runs]
            p95s = [r["p95_mean"] for r in runs]

            sm = f"{mean(spread_means):7.1f}±{ci95(spread_means):5.1f}" if spread_means else "N/A"
            sp = f"{mean(spread_p95s):7.1f}±{ci95(spread_p95s):5.1f}" if spread_p95s else "N/A"
            si = f"{mean(spike_isos):7.4f}±{ci95(spike_isos):5.4f}" if spike_isos else "N/A"
            dc = f"{mean(detrended):7.4f}±{ci95(detrended):5.4f}" if detrended else "N/A"
            rt = f"{mean(rates):7.0f}"
            p5 = f"{mean(p50s):7.0f}"
            p9 = f"{mean(p95s):7.0f}"

            print(f"{mode:<18} {loss:<6} {n:<5} {sm:<14} {sp:<14} {si:<12} {dc:<14} {rt:<12} {p5:<10} {p9:<10}")

    print()


def print_comparison(grouped: dict):
    losses_order = [0, 1, 2, 5]

    print("\n" + "=" * 80)
    print("CONTROL vs PER-TOPIC-FLOW COMPARISON")
    print("=" * 80)
    print("Ratios > 1.0 mean per-topic-flow has HIGHER values\n")

    print(f"{'Loss':<6} {'Spread ratio':<16} {'Spike iso ratio':<18} {'p50 ratio':<14} {'p95 ratio':<14}")
    print("-" * 70)

    for loss in losses_order:
        ctrl_key = ("control", loss)
        flow_key = ("per-topic-flow", loss)
        if ctrl_key not in grouped or flow_key not in grouped:
            continue

        ctrl_runs = grouped[ctrl_key]
        flow_runs = grouped[flow_key]

        ctrl_spread = mean([r["spread_mean"] for r in ctrl_runs if r["spread_mean"] is not None])
        flow_spread = mean([r["spread_mean"] for r in flow_runs if r["spread_mean"] is not None])
        spread_ratio = flow_spread / ctrl_spread if ctrl_spread > 0 else float("inf")

        ctrl_spike = mean([r["spike_iso"] for r in ctrl_runs if r["spike_iso"] is not None])
        flow_spike = mean([r["spike_iso"] for r in flow_runs if r["spike_iso"] is not None])
        spike_ratio = flow_spike / ctrl_spike if ctrl_spike > 0 else float("inf")

        ctrl_p50 = mean([r["p50_mean"] for r in ctrl_runs])
        flow_p50 = mean([r["p50_mean"] for r in flow_runs])
        p50_ratio = flow_p50 / ctrl_p50 if ctrl_p50 > 0 else float("inf")

        ctrl_p95 = mean([r["p95_mean"] for r in ctrl_runs])
        flow_p95 = mean([r["p95_mean"] for r in flow_runs])
        p95_ratio = flow_p95 / ctrl_p95 if ctrl_p95 > 0 else float("inf")

        print(f"{loss:<6} {spread_ratio:<16.2f} {spike_ratio:<18.4f} {p50_ratio:<14.3f} {p95_ratio:<14.3f}")

    print()
    print("Interpretation:")
    print("  Spread ratio > 1: per-topic-flow shows MORE inter-topic variation (expected HOL isolation)")
    print("  Spike iso ratio < 1: per-topic-flow has BETTER spike isolation (lower = more isolated)")
    print("  p50/p95 ratio < 1: per-topic-flow has LOWER latency")


def check_data_completeness(grouped: dict):
    modes_order = ["control", "per-topic-flow"]
    losses_order = [0, 1, 2, 5]
    expected_runs = 15

    print("\n" + "=" * 60)
    print("DATA COMPLETENESS")
    print("=" * 60)

    total_expected = len(modes_order) * len(losses_order) * expected_runs
    total_found = sum(len(runs) for runs in grouped.values())

    for mode in modes_order:
        for loss in losses_order:
            key = (mode, loss)
            n = len(grouped.get(key, []))
            status = "OK" if n == expected_runs else f"MISSING {expected_runs - n}"
            print(f"  {mode:<18} loss={loss}%: {n}/{expected_runs} runs  [{status}]")

    print(f"\n  Total: {total_found}/{total_expected} JSON files")


def main():
    if len(sys.argv) > 1:
        results_dir = Path(sys.argv[1])
    else:
        results_dir = Path("experiments/results-v5/07_subscriber_flow_delivery")

    if not results_dir.exists():
        print(f"Results directory not found: {results_dir}", file=sys.stderr)
        sys.exit(1)

    grouped = aggregate_by_condition(results_dir)
    if not grouped:
        print("No results found", file=sys.stderr)
        sys.exit(1)

    check_data_completeness(grouped)
    print_summary(grouped)
    print_comparison(grouped)


if __name__ == "__main__":
    main()
