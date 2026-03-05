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
    apply_style,
    save_figure,
)

TOPIC_SCALING_TRANSPORTS = ["tcp", "quic-pertopic", "quic-perpub"]
RUNS = range(1, 6)


def load_topic_scaling_data(results_dir: Path):
    exp02_dir = results_dir / "02_hol_blocking"
    exp02c_dir = results_dir / "02c_hol_topic_scaling"

    sources = {}

    if exp02_dir.exists():
        for transport in TOPIC_SCALING_TRANSPORTS:
            for run in RUNS:
                filepath = exp02_dir / f"{transport}_loss1pct_run{run}.json"
                if filepath.exists():
                    sources.setdefault(transport, {}).setdefault(8, [])
                    with open(filepath) as f:
                        result = json.load(f)
                    sources[transport][8].append(
                        result["results"]["spike_isolation_ratio"]
                    )

    if exp02c_dir.exists():
        for transport in TOPIC_SCALING_TRANSPORTS:
            for topic_count in [2, 4, 16, 32]:
                for run in RUNS:
                    filepath = (
                        exp02c_dir
                        / f"{transport}_{topic_count}topics_run{run}.json"
                    )
                    if filepath.exists():
                        sources.setdefault(transport, {}).setdefault(
                            topic_count, []
                        )
                        with open(filepath) as f:
                            result = json.load(f)
                        sources[transport][topic_count].append(
                            result["results"]["spike_isolation_ratio"]
                        )

    if not sources:
        print("  WARNING: no topic scaling data found, skipping fig03")
        return None

    return sources


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
    data = load_topic_scaling_data(results_dir)
    if data is None:
        return

    fig, ax = plt.subplots(figsize=(7, 4.5))

    for transport in TOPIC_SCALING_TRANSPORTS:
        if transport not in data:
            continue

        topic_counts = sorted(data[transport].keys())
        means = []
        ci_lows = []
        ci_highs = []

        for tc in topic_counts:
            mean, ci_half = compute_ci(data[transport][tc])
            means.append(mean)
            ci_lows.append(mean - ci_half)
            ci_highs.append(mean + ci_half)

        ax.plot(
            topic_counts,
            means,
            marker=TRANSPORT_MARKERS[transport],
            color=TRANSPORT_COLORS[transport],
            label=TRANSPORT_LABELS[transport],
            linewidth=1.5,
            markersize=6,
        )
        ax.fill_between(
            topic_counts,
            ci_lows,
            ci_highs,
            color=TRANSPORT_COLORS[transport],
            alpha=0.15,
        )

    ax.set_xlabel("Number of Topics")
    ax.set_ylabel("Spike Isolation Ratio")
    ax.set_title("HOL Blocking: Spike Isolation vs. Topic Count")
    ax.set_ylim(0, 1.05)
    ax.set_xscale("log", base=2)
    ax.set_xticks([2, 4, 8, 16, 32])
    ax.set_xticklabels(["2", "4", "8", "16", "32"])
    ax.legend(loc="best", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig03_spike_iso_vs_topics")


if __name__ == "__main__":
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("../../results")
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("./output")
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
