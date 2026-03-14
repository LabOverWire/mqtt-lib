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

PERCENTILES = ["p50", "p95", "p99"]
PERCENTILE_LABELS = ["p50", "p95", "p99"]
RUNS = range(1, 16)


def load_latency_data(results_dir: Path):
    exp02_dir = results_dir / "02_hol_blocking"
    if not exp02_dir.exists():
        print(f"  WARNING: {exp02_dir} not found, skipping fig05")
        return None

    data = {}
    for transport in TRANSPORT_ORDER:
        data[transport] = {p: [] for p in PERCENTILES}
        for run in RUNS:
            filepath = exp02_dir / f"{transport}_loss1pct_run{run}.json"
            if not filepath.exists():
                continue
            with open(filepath) as f:
                result = json.load(f)

            topics = result["results"]["topics"]
            for percentile in PERCENTILES:
                key = f"{percentile}_us"
                topic_values = [t[key] for t in topics if key in t]
                if topic_values:
                    run_mean = np.mean(topic_values)
                    data[transport][percentile].append(run_mean)

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
    data = load_latency_data(results_dir)
    if data is None:
        return

    fig, ax = plt.subplots(figsize=(7, 4.5))

    num_transports = len(TRANSPORT_ORDER)
    num_percentiles = len(PERCENTILES)
    bar_width = 0.18
    group_positions = np.arange(num_percentiles)

    for transport_idx, transport in enumerate(TRANSPORT_ORDER):
        means = []
        ci_widths = []
        for percentile in PERCENTILES:
            values = data[transport][percentile]
            if values:
                mean, ci_half = compute_ci(values)
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

    ax.set_xlabel("Latency Percentile")
    ax.set_ylabel("Latency (ms)")
    ax.set_title("Latency Percentiles at 1% Loss (8 Topics)")
    ax.set_xticks(group_positions)
    ax.set_xticklabels(PERCENTILE_LABELS)

    ax.yaxis.set_major_formatter(plt.FuncFormatter(lambda x, _: f"{x / 1000:.0f}"))
    ax.set_yticks(np.arange(0, 80001, 10000))
    ax.set_ylim(0, 80000)
    ax.grid(axis="y", alpha=0.3)

    ax.legend(loc="best", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig05_latency_percentiles")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results-v5"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
