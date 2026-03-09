import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

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
RUNS = range(1, 16)


def load_all_runs(results_dir: Path):
    exp02_dir = results_dir / "02_hol_blocking"
    if not exp02_dir.exists():
        print("  WARNING: 02_hol_blocking not found, skipping fig04")
        return None

    data = {}
    for transport in TRANSPORT_ORDER:
        data[transport] = {}
        for loss in LOSS_RATES:
            values = []
            for run in RUNS:
                filepath = exp02_dir / f"{transport}_loss{loss}pct_run{run}.json"
                if filepath.exists():
                    with open(filepath) as f:
                        result = json.load(f)
                    values.append(result["results"]["windowed_correlation"])
            if values:
                data[transport][loss] = values

    return data


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_all_runs(results_dir)
    if data is None:
        return

    fig, ax = plt.subplots(figsize=(7, 5.5))

    num_transports = len(TRANSPORT_ORDER)
    bar_width = 0.18
    group_positions = np.arange(len(LOSS_RATES))

    for t_idx, transport in enumerate(TRANSPORT_ORDER):
        means = []
        lows = []
        highs = []
        for loss in LOSS_RATES:
            vals = data[transport].get(loss, [])
            if vals:
                arr = np.array(vals)
                means.append(np.median(arr))
                lows.append(np.percentile(arr, 25))
                highs.append(np.percentile(arr, 75))
            else:
                means.append(0)
                lows.append(0)
                highs.append(0)

        offset = (t_idx - (num_transports - 1) / 2) * bar_width
        positions = group_positions + offset
        yerr_low = [m - lo for m, lo in zip(means, lows)]
        yerr_high = [hi - m for m, hi in zip(means, highs)]

        ax.bar(
            positions,
            means,
            bar_width,
            yerr=[yerr_low, yerr_high],
            capsize=3,
            color=TRANSPORT_COLORS[transport],
            label=TRANSPORT_LABELS[transport],
            edgecolor="white",
            linewidth=0.5,
            alpha=0.85,
        )

    ax.set_xlabel("Packet Loss Rate")
    ax.set_ylabel("Windowed Correlation (median, IQR)")
    ax.set_title("Latency Correlation Across Topics by Transport and Loss")
    ax.set_xticks(group_positions)
    ax.set_xticklabels(LOSS_LABELS)
    ax.set_ylim(0, 1.12)
    ax.axhline(y=1.0, color="gray", linewidth=0.5, linestyle="--", zorder=1)
    ax.legend(loc="lower right", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig04_wcorr_vs_spike_iso")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results_v2"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
