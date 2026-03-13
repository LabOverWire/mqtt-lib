import csv
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

CONCURRENCIES = [10, 50, 100]
RUNS = range(1, 16)


def compute_ci(values, confidence=0.95):
    n = len(values)
    if n < 2:
        return np.mean(values), 0.0
    m = np.mean(values)
    sem = stats.sem(values)
    t_crit = stats.t.ppf((1 + confidence) / 2, df=n - 1)
    return m, t_crit * sem


def peak_rss_from_csv(csv_path: Path) -> float:
    peak = 0.0
    with open(csv_path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            rss = float(row["rss_kb"])
            if rss > peak:
                peak = rss
    return peak / 1024.0


def load_data(results_dir: Path):
    exp_dir = results_dir / "06_resource_overhead"
    if not exp_dir.exists():
        print(f"  WARNING: {exp_dir} not found, skipping fig13")
        return None

    data = {}
    for transport in THROUGHPUT_ORDER:
        data[transport] = {"throughput": {}, "peak_rss": {}}
        for conns in CONCURRENCIES:
            tp_values = []
            rss_values = []
            for run in RUNS:
                json_path = exp_dir / f"{transport}_{conns}conn_run{run}.json"
                csv_path = exp_dir / f"{transport}_{conns}conn_run{run}_broker_resources.csv"

                if json_path.exists():
                    with open(json_path) as f:
                        d = json.load(f)
                    tp_values.append(d["results"]["throughput_avg"])

                if csv_path.exists():
                    rss_values.append(peak_rss_from_csv(csv_path))

            if tp_values:
                data[transport]["throughput"][conns] = tp_values
            if rss_values:
                data[transport]["peak_rss"][conns] = rss_values
    return data


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_data(results_dir)
    if data is None:
        return

    fig, ax_tp = plt.subplots(figsize=(5, 3.5))

    for transport in THROUGHPUT_ORDER:
        x_vals = []
        y_means = []
        y_errs = []
        for conns in CONCURRENCIES:
            tp_data = data[transport]["throughput"].get(conns)
            if tp_data:
                m, e = compute_ci(tp_data)
                x_vals.append(conns)
                y_means.append(m / 1000.0)
                y_errs.append(e / 1000.0)

        if not x_vals:
            continue

        ax_tp.errorbar(
            x_vals, y_means, yerr=y_errs,
            marker=THROUGHPUT_MARKERS[transport],
            color=THROUGHPUT_COLORS[transport],
            label=THROUGHPUT_LABELS[transport],
            linewidth=1.5, markersize=6, capsize=4,
        )

    ax_tp.set_xlabel("Connections")
    ax_tp.set_ylabel("Throughput (K msgs/sec)")
    ax_tp.set_title("Throughput vs. Connection Count")
    ax_tp.set_xticks(CONCURRENCIES)
    ax_tp.legend(loc="best", fontsize=8)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig13_resource_overhead_scaled")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results-v5"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
