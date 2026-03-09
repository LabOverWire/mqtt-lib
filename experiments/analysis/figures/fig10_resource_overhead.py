import csv
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

RUNS = range(1, 6)


def load_resource_csvs(results_dir: Path):
    exp_dir = results_dir / "02_hol_blocking"
    if not exp_dir.exists():
        print(f"  WARNING: {exp_dir} not found, skipping fig10")
        return None

    data = {}
    for transport in TRANSPORT_ORDER:
        run_series = []
        for run in RUNS:
            filepath = exp_dir / f"{transport}_loss1pct_run{run}_broker_resources.csv"
            if not filepath.exists():
                continue
            timestamps = []
            rss_kb = []
            cpu_pct = []
            with open(filepath) as f:
                reader = csv.DictReader(f)
                for row in reader:
                    timestamps.append(float(row["timestamp"]))
                    rss_kb.append(float(row["rss_kb"]))
                    cpu_pct.append(float(row["cpu_percent"]))
            if timestamps:
                t0 = timestamps[0]
                elapsed = [t - t0 for t in timestamps]
                rss_mb = [v / 1024.0 for v in rss_kb]
                run_series.append({"elapsed": elapsed, "rss_mb": rss_mb, "cpu_pct": cpu_pct})
        if run_series:
            data[transport] = run_series
    return data


def interpolate_runs(run_series):
    max_t = min(s["elapsed"][-1] for s in run_series)
    common_t = np.arange(0, max_t + 1, 1.0)

    rss_matrix = []
    cpu_matrix = []
    for series in run_series:
        rss_interp = np.interp(common_t, series["elapsed"], series["rss_mb"])
        cpu_interp = np.interp(common_t, series["elapsed"], series["cpu_pct"])
        rss_matrix.append(rss_interp)
        cpu_matrix.append(cpu_interp)

    return common_t, np.array(rss_matrix), np.array(cpu_matrix)


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_resource_csvs(results_dir)
    if data is None:
        return

    fig, axes = plt.subplots(2, 1, figsize=(7, 7), sharex=True)

    for transport in TRANSPORT_ORDER:
        if transport not in data:
            continue

        common_t, rss_matrix, cpu_matrix = interpolate_runs(data[transport])

        rss_median = np.median(rss_matrix, axis=0)
        rss_q25 = np.percentile(rss_matrix, 25, axis=0)
        rss_q75 = np.percentile(rss_matrix, 75, axis=0)

        cpu_median = np.median(cpu_matrix, axis=0)
        cpu_q25 = np.percentile(cpu_matrix, 25, axis=0)
        cpu_q75 = np.percentile(cpu_matrix, 75, axis=0)

        color = TRANSPORT_COLORS[transport]
        label = TRANSPORT_LABELS[transport]

        axes[0].plot(common_t, rss_median, color=color, label=label, linewidth=1.2)
        axes[0].fill_between(common_t, rss_q25, rss_q75, alpha=0.15, color=color)

        axes[1].plot(common_t, cpu_median, color=color, label=label, linewidth=1.2)
        axes[1].fill_between(common_t, cpu_q25, cpu_q75, alpha=0.15, color=color)

    axes[0].set_ylabel("RSS (MB)")
    axes[0].set_title("Broker Memory Usage at 1% Loss")
    axes[0].legend(loc="best", framealpha=0.9)

    axes[1].set_ylabel("CPU (%)")
    axes[1].set_xlabel("Elapsed Time (s)")
    axes[1].set_title("Broker CPU Usage at 1% Loss")
    axes[1].legend(loc="best", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig10_resource_overhead")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results_v2"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
