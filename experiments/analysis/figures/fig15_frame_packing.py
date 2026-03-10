#!/usr/bin/env python3
"""Fig 15: Frame packing comparison — Greedy vs StreamIsolated.

2x2 grid: (per-topic, per-publish) x (inter-topic spread, windowed correlation)
Each panel shows Greedy (v3) and StreamIsolated (v4) as separate lines,
x-axis = loss rate, error bars = 95% CI.
"""

import json
import re
import sys
from collections import defaultdict
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats

sys.path.insert(0, str(Path(__file__).parent))
from style import apply_style, save_figure

LOSS_RATES = [0, 1, 2, 5]
LOSS_LABELS = ["0%", "1%", "2%", "5%"]
RUNS = range(1, 16)

PACKING_STYLES = {
    "greedy": {"color": "#1f77b4", "marker": "o", "label": "Greedy (default)"},
    "isolated": {"color": "#d62728", "marker": "^", "label": "StreamIsolated"},
}

STRATEGY_CONFIGS = {
    "per-topic": {
        "greedy_key": "quic-pertopic",
        "isolated_key": "quic-pertopic-isolated",
        "title": "Per-Topic Stream Strategy",
    },
    "per-publish": {
        "greedy_key": "quic-perpub",
        "isolated_key": "quic-perpub-isolated",
        "title": "Per-Publish Stream Strategy",
    },
}


def parse_filename(name: str) -> tuple[str, int, int] | None:
    m = re.match(r"(.+?)_loss(\d+)pct_run(\d+)\.json", name)
    if m:
        return m.group(1), int(m.group(2)), int(m.group(3))
    return None


def load_results(results_dir: Path, transport_filter: list[str]) -> dict:
    grouped = defaultdict(list)
    for f in sorted(results_dir.glob("*.json")):
        if "_pub.json" in f.name:
            continue
        parsed = parse_filename(f.name)
        if not parsed:
            continue
        transport, loss_pct, _run = parsed
        if transport not in transport_filter:
            continue
        try:
            with open(f) as fh:
                data = json.load(fh)
            results = data["results"]
            grouped[(transport, loss_pct)].append({
                "spread_mean": results.get("inter_topic_spread_mean_us"),
                "wcorr": results["windowed_correlation"],
                "detrended_corr": results.get("detrended_correlation"),
            })
        except (json.JSONDecodeError, KeyError):
            pass
    return grouped


def compute_ci(values, confidence=0.95):
    n = len(values)
    if n < 2:
        return np.mean(values), 0.0
    m = np.mean(values)
    sem = stats.sem(values)
    t_crit = stats.t.ppf((1 + confidence) / 2, df=n - 1)
    return m, t_crit * sem


def plot_panel(ax, greedy_data, isolated_data, greedy_key, isolated_key,
               metric_key, ylabel, title):
    group_positions = np.arange(len(LOSS_RATES))

    for packing, data, key, offset in [
        ("greedy", greedy_data, greedy_key, -0.08),
        ("isolated", isolated_data, isolated_key, 0.08),
    ]:
        style = PACKING_STYLES[packing]
        means, ci_halves, positions = [], [], []

        for loss_idx, loss in enumerate(LOSS_RATES):
            vals = [r[metric_key] for r in data.get((key, loss), [])
                    if r[metric_key] is not None]
            if vals:
                m, ci_half = compute_ci(vals)
                if metric_key == "spread_mean":
                    m /= 1000.0
                    ci_half /= 1000.0
                means.append(m)
                ci_halves.append(ci_half)
                positions.append(group_positions[loss_idx] + offset)

        if means:
            ax.errorbar(
                positions, means, yerr=ci_halves,
                fmt=style["marker"] + "-",
                color=style["color"],
                label=style["label"],
                markersize=7, capsize=4, capthick=1.5,
                linewidth=1.5, elinewidth=1.5,
                markeredgecolor="white", markeredgewidth=0.8,
                zorder=3,
            )

    ax.set_title(title)
    ax.set_ylabel(ylabel)
    ax.set_xticks(group_positions)
    ax.set_xticklabels(LOSS_LABELS)
    ax.set_xlabel("Packet Loss Rate")


def main(v3_dir: Path, v4_dir: Path, output_dir: Path):
    apply_style()

    v3_pertopic = load_results(v3_dir, ["quic-pertopic"])
    v3_perpub = load_results(v3_dir, ["quic-perpub"])
    v4_pertopic = load_results(v4_dir, ["quic-pertopic-isolated"])
    v4_perpub = load_results(v4_dir, ["quic-perpub-isolated"])

    has_v4 = bool(v4_pertopic or v4_perpub)
    if not has_v4:
        print("  WARNING: no v4 data found, skipping fig15")
        return

    fig, axes = plt.subplots(2, 2, figsize=(12, 8))

    for col, (strategy, cfg) in enumerate(STRATEGY_CONFIGS.items()):
        greedy_data = v3_pertopic if strategy == "per-topic" else v3_perpub
        isolated_data = v4_pertopic if strategy == "per-topic" else v4_perpub

        plot_panel(
            axes[0, col], greedy_data, isolated_data,
            cfg["greedy_key"], cfg["isolated_key"],
            "spread_mean", "Inter-Topic Spread (ms)",
            cfg["title"],
        )

        plot_panel(
            axes[1, col], greedy_data, isolated_data,
            cfg["greedy_key"], cfg["isolated_key"],
            "wcorr", "Windowed Correlation",
            cfg["title"],
        )
    axes[1, 0].set_ylim(0.4, 1.05)
    axes[1, 1].set_ylim(0.6, 1.05)

    axes[0, 0].set_ylim(bottom=0)
    axes[0, 1].set_ylim(bottom=0)
    axes[0, 0].legend(loc="upper left", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig15_frame_packing")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    base = script_dir.parent.parent
    v3_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else base / "results_v3" / "02_hol_blocking"
    v4_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else base / "results_v4" / "02_hol_blocking"
    output_dir = Path(sys.argv[3]) if len(sys.argv) > 3 else script_dir / "output"
    output_dir.mkdir(parents=True, exist_ok=True)
    main(v3_dir, v4_dir, output_dir)
