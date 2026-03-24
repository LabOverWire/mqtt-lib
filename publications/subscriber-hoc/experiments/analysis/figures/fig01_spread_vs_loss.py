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
    MODE_MARKERS,
    MODE_ORDER,
    RUNS,
    apply_style,
    save_figure,
)


def load_data(results_dir: Path):
    data = {}
    for mode in MODE_ORDER:
        data[mode] = {}
        for loss in LOSS_RATES:
            values = []
            for run in RUNS:
                filepath = results_dir / f"{mode}_loss{loss}pct_run{run}.json"
                if filepath.exists() and filepath.stat().st_size > 10:
                    with open(filepath) as f:
                        result = json.load(f)
                    val = result["results"].get("inter_topic_spread_mean_us")
                    if val is not None:
                        values.append(val / 1000.0)
            if values:
                data[mode][loss] = values
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
        print("  WARNING: no data found, skipping fig01")
        return

    fig, ax = plt.subplots(figsize=(7, 4.5))
    x_offsets = {"control": -0.08, "per-topic-flow": 0.08}
    group_positions = np.arange(len(LOSS_RATES))

    for mode in MODE_ORDER:
        means, ci_halves, positions = [], [], []
        for loss_idx, loss in enumerate(LOSS_RATES):
            if loss in data[mode]:
                mean_val, ci_half = compute_ci(data[mode][loss])
                means.append(mean_val)
                ci_halves.append(ci_half)
                positions.append(group_positions[loss_idx] + x_offsets[mode])

        ax.errorbar(
            positions, means, yerr=ci_halves,
            fmt=MODE_MARKERS[mode] + "-",
            color=MODE_COLORS[mode],
            label=MODE_LABELS[mode],
            markersize=8, capsize=4, capthick=1.5,
            linewidth=1.5, elinewidth=1.5,
            markeredgecolor="white", markeredgewidth=0.8, zorder=3,
        )

    ax.set_xlabel("Packet Loss Rate")
    ax.set_ylabel("Inter-Topic Spread (ms)")
    ax.set_xticks(group_positions)
    ax.set_xticklabels(LOSS_LABELS)
    ax.set_ylim(bottom=0)
    ax.legend(loc="upper left", framealpha=0.9)
    fig.tight_layout()
    save_figure(fig, output_dir, "fig01_spread_vs_loss")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results" / "01_subscriber_flow_delivery"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
