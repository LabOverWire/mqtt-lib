import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats

sys.path.insert(0, str(Path(__file__).parent))
from style import (
    CONN_TRANSPORT_COLORS,
    CONN_TRANSPORT_LABELS,
    CONN_TRANSPORT_ORDER,
    apply_style,
    save_figure,
)

DELAYS_MS = [0, 25, 50, 100, 200]
DELAY_LABELS = ["0ms", "25ms", "50ms", "100ms", "200ms"]
RUNS = range(1, 6)


def load_connection_latency(results_dir: Path):
    exp_dir = results_dir / "01_connection_latency"
    if not exp_dir.exists():
        print(f"  WARNING: {exp_dir} not found, skipping fig08")
        return None

    data = {}
    for transport in CONN_TRANSPORT_ORDER:
        data[transport] = {}
        for delay in DELAYS_MS:
            p50_values = []
            p95_values = []
            for run in RUNS:
                filepath = exp_dir / f"{transport}_delay{delay}ms_run{run}.json"
                if not filepath.exists():
                    continue
                with open(filepath) as f:
                    result = json.load(f)
                p50_values.append(result["results"]["p50_connect_us"])
                p95_values.append(result["results"]["p95_connect_us"])
            if p50_values:
                data[transport][delay] = {"p50": p50_values, "p95": p95_values}
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
    data = load_connection_latency(results_dir)
    if data is None:
        return

    fig, ax = plt.subplots(figsize=(7, 4.5))

    num_transports = len(CONN_TRANSPORT_ORDER)
    num_groups = len(DELAYS_MS)
    bar_width = 0.22
    group_positions = np.arange(num_groups)

    for tidx, transport in enumerate(CONN_TRANSPORT_ORDER):
        p50_means = []
        p50_cis = []
        p95_tops = []
        for delay in DELAYS_MS:
            if delay in data[transport]:
                p50_mean, p50_ci = compute_ci(data[transport][delay]["p50"])
                p95_mean, _ = compute_ci(data[transport][delay]["p95"])
                p50_means.append(p50_mean)
                p50_cis.append(p50_ci)
                p95_tops.append(max(0, p95_mean - p50_mean))
            else:
                p50_means.append(0)
                p50_cis.append(0)
                p95_tops.append(0)

        offset = (tidx - (num_transports - 1) / 2) * bar_width
        positions = group_positions + offset

        ax.bar(
            positions,
            p50_means,
            bar_width,
            yerr=[[0] * num_groups, p95_tops],
            capsize=3,
            color=CONN_TRANSPORT_COLORS[transport],
            label=CONN_TRANSPORT_LABELS[transport],
            edgecolor="white",
            linewidth=0.5,
        )

    ax.set_xlabel("One-Way Delay (RTT = 2x)")
    ax.set_ylabel("Connect Latency (us)")
    ax.set_yscale("log")
    ax.set_title("Connection Setup Latency vs. Network Delay")
    ax.set_xticks(group_positions)
    ax.set_xticklabels(DELAY_LABELS)
    ax.legend(loc="upper left", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig08_connection_latency")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
