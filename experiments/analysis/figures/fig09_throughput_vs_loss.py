import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats

sys.path.insert(0, str(Path(__file__).parent))
from style import (
    THROUGHPUT_COLORS,
    THROUGHPUT_LABELS,
    THROUGHPUT_MARKERS,
    THROUGHPUT_ORDER,
    apply_style,
    save_figure,
)

LOSS_RATES = [0, 1, 2, 5, 10]
LOSS_LABELS = ["0%", "1%", "2%", "5%", "10%"]
QOS_LEVELS = [0, 1]
RUNS = range(1, 16)


def load_throughput_data(results_dir: Path):
    exp_dir = results_dir / "03_throughput_under_loss"
    if not exp_dir.exists():
        print(f"  WARNING: {exp_dir} not found, skipping fig09")
        return None

    data = {}
    for strategy in THROUGHPUT_ORDER:
        data[strategy] = {}
        for qos in QOS_LEVELS:
            data[strategy][qos] = {}
            for loss in LOSS_RATES:
                values = []
                for run in RUNS:
                    filepath = exp_dir / f"{strategy}_qos{qos}_loss{loss}pct_run{run}.json"
                    if not filepath.exists():
                        continue
                    with open(filepath) as f:
                        result = json.load(f)
                    values.append(result["results"]["throughput_avg"])
                if values:
                    data[strategy][qos][loss] = values
    return data


def compute_ci(values, confidence=0.95):
    n = len(values)
    if n < 2:
        return np.mean(values), 0.0
    m = np.mean(values)
    sem = stats.sem(values)
    t_crit = stats.t.ppf((1 + confidence) / 2, df=n - 1)
    return m, t_crit * sem


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_throughput_data(results_dir)
    if data is None:
        return

    fig, axes = plt.subplots(2, 1, figsize=(7, 8), sharex=True)

    for qos_idx, qos in enumerate(QOS_LEVELS):
        ax = axes[qos_idx]
        for strategy in THROUGHPUT_ORDER:
            x_vals = []
            y_means = []
            y_lo = []
            y_hi = []
            for loss in LOSS_RATES:
                if loss in data[strategy][qos]:
                    m, ci_half = compute_ci(data[strategy][qos][loss])
                    x_vals.append(loss)
                    y_means.append(m)
                    y_lo.append(m - ci_half)
                    y_hi.append(m + ci_half)

            if not x_vals:
                continue

            ax.plot(
                x_vals,
                y_means,
                marker=THROUGHPUT_MARKERS[strategy],
                color=THROUGHPUT_COLORS[strategy],
                label=THROUGHPUT_LABELS[strategy],
                linewidth=1.5,
                markersize=5,
            )
            ax.fill_between(
                x_vals,
                y_lo,
                y_hi,
                alpha=0.15,
                color=THROUGHPUT_COLORS[strategy],
            )

        ax.set_ylabel("Throughput (msgs/sec)")
        ax.set_title(f"QoS {qos}")
        ax.legend(loc="best", framealpha=0.9)

    axes[1].set_xlabel("Packet Loss Rate (%)")
    axes[1].set_xticks(LOSS_RATES)
    axes[1].set_xticklabels(LOSS_LABELS)
    fig.suptitle("Throughput vs. Packet Loss Rate", fontsize=12, y=0.98)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig09_throughput_vs_loss")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results-v5"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
