import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats

sys.path.insert(0, str(Path(__file__).parent))
from style import (
    STRATEGY_COLORS,
    STRATEGY_LABELS,
    STRATEGY_MARKERS,
    STRATEGY_ORDER,
    apply_style,
    save_figure,
)

TOPIC_COUNTS = [1, 4, 8, 16]
RUNS = range(1, 6)


def compute_ci(values, confidence=0.95):
    n = len(values)
    if n < 2:
        return np.mean(values), 0.0
    m = np.mean(values)
    sem = stats.sem(values)
    t_crit = stats.t.ppf((1 + confidence) / 2, df=n - 1)
    return m, t_crit * sem


def load_data(results_dir: Path):
    exp_dir = results_dir / "04_stream_strategies"
    if not exp_dir.exists():
        print(f"  WARNING: {exp_dir} not found, skipping fig14")
        return None

    data = {}
    for strategy in STRATEGY_ORDER:
        data[strategy] = {"latency": {}, "throughput": {}}
        for topics in TOPIC_COUNTS:
            lat_values = []
            tp_values = []
            for run in RUNS:
                lat_path = exp_dir / f"{strategy}_{topics}topics_latency_run{run}.json"
                if lat_path.exists():
                    with open(lat_path) as f:
                        d = json.load(f)
                    lat_values.append(d["results"]["p50_us"])

                tp_path = exp_dir / f"{strategy}_{topics}topics_throughput_run{run}.json"
                if tp_path.exists():
                    with open(tp_path) as f:
                        d = json.load(f)
                    tp_values.append(d["results"]["throughput_avg"])

            if lat_values:
                data[strategy]["latency"][topics] = lat_values
            if tp_values:
                data[strategy]["throughput"][topics] = tp_values
    return data


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_data(results_dir)
    if data is None:
        return

    fig, axes = plt.subplots(1, 2, figsize=(7, 3.5))

    ax_lat = axes[0]
    for strategy in STRATEGY_ORDER:
        x_vals = []
        y_means = []
        y_errs = []
        for topics in TOPIC_COUNTS:
            lat_data = data[strategy]["latency"].get(topics)
            if lat_data:
                m, e = compute_ci(lat_data)
                x_vals.append(topics)
                y_means.append(m / 1000.0)
                y_errs.append(e / 1000.0)

        if not x_vals:
            continue

        ax_lat.errorbar(
            x_vals, y_means, yerr=y_errs,
            marker=STRATEGY_MARKERS[strategy],
            color=STRATEGY_COLORS[strategy],
            label=STRATEGY_LABELS[strategy],
            linewidth=1.5, markersize=6, capsize=4,
        )

    ax_lat.set_xlabel("Topic Count")
    ax_lat.set_ylabel("p50 Latency (ms)")
    ax_lat.set_title("(a) Latency vs. Topic Count")
    ax_lat.set_xticks(TOPIC_COUNTS)
    ax_lat.legend(loc="best", fontsize=8)

    ax_tp = axes[1]
    for strategy in STRATEGY_ORDER:
        x_vals = []
        y_means = []
        y_errs = []
        for topics in TOPIC_COUNTS:
            tp_data = data[strategy]["throughput"].get(topics)
            if tp_data:
                m, e = compute_ci(tp_data)
                x_vals.append(topics)
                y_means.append(m / 1000.0)
                y_errs.append(e / 1000.0)

        if not x_vals:
            continue

        ax_tp.errorbar(
            x_vals, y_means, yerr=y_errs,
            marker=STRATEGY_MARKERS[strategy],
            color=STRATEGY_COLORS[strategy],
            label=STRATEGY_LABELS[strategy],
            linewidth=1.5, markersize=6, capsize=4,
        )

    ax_tp.set_xlabel("Topic Count")
    ax_tp.set_ylabel("Throughput (K msgs/sec)")
    ax_tp.set_title("(b) Throughput vs. Topic Count")
    ax_tp.set_xticks(TOPIC_COUNTS)
    ax_tp.legend(loc="best", fontsize=8)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig14_strategy_comparison")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results_v2"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
