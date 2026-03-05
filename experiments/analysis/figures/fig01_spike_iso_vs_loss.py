import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats

sys.path.insert(0, str(Path(__file__).parent))
from style import (
    TRANSPORT_COLORS,
    TRANSPORT_LABELS,
    TRANSPORT_ORDER,
    apply_style,
    save_figure,
)

LOSS_RATES = [0, 1, 2, 5]
LOSS_LABELS = ["0%", "1%", "2%", "5%"]
RUNS = range(1, 6)


def load_spike_isolation_by_loss(results_dir: Path):
    exp02_dir = results_dir / "02_hol_blocking"
    if not exp02_dir.exists():
        print(f"  WARNING: {exp02_dir} not found, skipping fig01")
        return None

    data = {}
    for transport in TRANSPORT_ORDER:
        data[transport] = {}
        for loss in LOSS_RATES:
            values = []
            for run in RUNS:
                filename = f"{transport}_loss{loss}pct_run{run}.json"
                filepath = exp02_dir / filename
                if filepath.exists():
                    with open(filepath) as f:
                        result = json.load(f)
                    values.append(result["results"]["spike_isolation_ratio"])
            if values:
                data[transport][loss] = values
    return data


def compute_ci(values, confidence=0.95):
    n = len(values)
    if n < 2:
        return np.mean(values), 0.0
    mean = np.mean(values)
    sem = stats.sem(values)
    t_crit = stats.t.ppf((1 + confidence) / 2, df=n - 1)
    return mean, t_crit * sem


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_spike_isolation_by_loss(results_dir)
    if data is None:
        return

    fig, ax = plt.subplots(figsize=(7, 4.5))

    num_transports = len(TRANSPORT_ORDER)
    num_groups = len(LOSS_RATES)
    bar_width = 0.18
    group_positions = np.arange(num_groups)

    for transport_idx, transport in enumerate(TRANSPORT_ORDER):
        means = []
        ci_widths = []
        for loss in LOSS_RATES:
            if loss in data[transport]:
                mean, ci_half = compute_ci(data[transport][loss])
                means.append(mean)
                ci_widths.append(ci_half)
            else:
                means.append(0)
                ci_widths.append(0)

        offset = (transport_idx - (num_transports - 1) / 2) * bar_width
        positions = group_positions + offset

        ax.bar(
            positions,
            means,
            bar_width,
            yerr=ci_widths,
            capsize=3,
            color=TRANSPORT_COLORS[transport],
            label=TRANSPORT_LABELS[transport],
            edgecolor="white",
            linewidth=0.5,
        )

    ax.set_xlabel("Packet Loss Rate")
    ax.set_ylabel("Spike Isolation Ratio")
    ax.set_title("HOL Blocking: Spike Isolation vs. Loss Rate")
    ax.set_xticks(group_positions)
    ax.set_xticklabels(LOSS_LABELS)
    ax.set_ylim(0, 1.05)
    ax.legend(loc="best", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig01_spike_iso_vs_loss")


if __name__ == "__main__":
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("../../results")
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("./output")
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
