import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats

sys.path.insert(0, str(Path(__file__).parent))
from style import (
    LOSS_LABELS,
    LOSS_RATES,
    MODE_COLORS,
    MODE_LABELS,
    MODE_ORDER,
    RUNS,
    apply_style,
    save_figure,
)


def load_data(results_dir: Path):
    data = {}
    for mode in MODE_ORDER:
        data[mode] = {"p50": {}, "p95": {}}
        for loss in LOSS_RATES:
            p50_vals, p95_vals = [], []
            for run in RUNS:
                filepath = results_dir / f"{mode}_loss{loss}pct_run{run}.json"
                if filepath.exists() and filepath.stat().st_size > 10:
                    with open(filepath) as f:
                        result = json.load(f)
                    topics = result["results"]["topics"]
                    p50s = [t["p50_us"] for t in topics]
                    p95s = [t["p95_us"] for t in topics]
                    p50_vals.append(np.mean(p50s) / 1000.0)
                    p95_vals.append(np.mean(p95s) / 1000.0)
            if p50_vals:
                data[mode]["p50"][loss] = p50_vals
                data[mode]["p95"][loss] = p95_vals
    return data


def compute_ci(values, confidence=0.95):
    n = len(values)
    if n < 2:
        return np.mean(values), 0.0
    mean_val = np.mean(values)
    sem = stats.sem(values)
    t_crit = stats.t.ppf((1 + confidence) / 2, df=n - 1)
    return mean_val, t_crit * sem


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_data(results_dir)
    if not data:
        print("  WARNING: no data found, skipping fig03")
        return

    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(12, 4.5), sharey=False)
    group_positions = np.arange(len(LOSS_RATES))

    for percentile, ax, title in [("p50", ax1, "Median Latency (p50)"), ("p95", ax2, "Tail Latency (p95)")]:
        x_offsets = {"control": -0.08, "per-topic-flow": 0.08}
        bar_width = 0.35

        for mode in MODE_ORDER:
            means, ci_halves, positions = [], [], []
            for loss_idx, loss in enumerate(LOSS_RATES):
                if loss in data[mode][percentile]:
                    mean_val, ci_half = compute_ci(data[mode][percentile][loss])
                    means.append(mean_val)
                    ci_halves.append(ci_half)
                    positions.append(group_positions[loss_idx] + x_offsets[mode])

            ax.bar(
                positions, means, bar_width,
                yerr=ci_halves,
                color=MODE_COLORS[mode], label=MODE_LABELS[mode],
                alpha=0.85, capsize=3, edgecolor="white", linewidth=0.5,
                error_kw={"elinewidth": 1.2, "capthick": 1.2},
            )

        ax.set_xlabel("Packet Loss Rate")
        ax.set_ylabel("Latency (ms)")
        ax.set_title(title)
        ax.set_xticks(group_positions)
        ax.set_xticklabels(LOSS_LABELS)
        ax.set_ylim(bottom=0)
        ax.legend(loc="upper left", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig03_latency_percentiles")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results" / "01_subscriber_flow_delivery"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
