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
    TRANSPORT_MARKERS,
    TRANSPORT_ORDER,
    apply_style,
    save_figure,
)

LOSS_RATES = [0, 1, 2, 5]
LOSS_LABELS = ["0%", "1%", "2%", "5%"]
RUNS = range(1, 16)


def load_wcorr_by_loss(results_dir: Path):
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
                    values.append(result["results"]["windowed_correlation"])
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
    data = load_wcorr_by_loss(results_dir)
    if data is None:
        return

    fig, ax = plt.subplots(figsize=(7, 4.5))

    x_offsets = {
        "tcp": -0.15,
        "quic-control": -0.05,
        "quic-pertopic": 0.05,
        "quic-perpub": 0.15,
    }

    group_positions = np.arange(len(LOSS_RATES))

    for transport in TRANSPORT_ORDER:
        means = []
        ci_halves = []
        positions = []
        for loss_idx, loss in enumerate(LOSS_RATES):
            if loss in data[transport]:
                mean, ci_half = compute_ci(data[transport][loss])
                means.append(mean)
                ci_halves.append(ci_half)
                positions.append(group_positions[loss_idx] + x_offsets[transport])

        ax.errorbar(
            positions,
            means,
            yerr=ci_halves,
            fmt=TRANSPORT_MARKERS[transport],
            color=TRANSPORT_COLORS[transport],
            label=TRANSPORT_LABELS[transport],
            markersize=8,
            capsize=4,
            capthick=1.5,
            linewidth=0,
            elinewidth=1.5,
            markeredgecolor="white",
            markeredgewidth=0.8,
            zorder=3,
        )

    ax.set_xlabel("Packet Loss Rate")
    ax.set_ylabel("Windowed Correlation")
    ax.set_xticks(group_positions)
    ax.set_xticklabels(LOSS_LABELS)
    ax.set_ylim(-0.05, 1.15)
    ax.axhline(y=1.0, color="gray", linewidth=0.5, linestyle="--", zorder=1)
    ax.legend(loc="lower right", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig01_spike_iso_vs_loss")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results_v2"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
