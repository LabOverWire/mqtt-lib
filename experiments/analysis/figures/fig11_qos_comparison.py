import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats

sys.path.insert(0, str(Path(__file__).parent))
from style import apply_style, save_figure

TRANSPORTS = ["tcp", "quic-pertopic"]
TRANSPORT_LABELS = {"tcp": "TCP", "quic-pertopic": "QUIC per-topic"}
QOS_COLORS = {"qos0": "#1f77b4", "qos1": "#d62728"}
QOS_LABELS = {"qos0": "QoS 0", "qos1": "QoS 1"}
RUNS = range(1, 6)


def load_qos_data(results_dir: Path):
    qos0_dir = results_dir / "02_hol_blocking"
    qos1_dir = results_dir / "02e_hol_qos1"

    if not qos0_dir.exists() or not qos1_dir.exists():
        print(f"  WARNING: need both {qos0_dir} and {qos1_dir}, skipping fig11")
        return None

    data = {}
    for transport in TRANSPORTS:
        data[transport] = {"qos0": {"spike_iso": [], "measured_rate": []},
                           "qos1": {"spike_iso": [], "measured_rate": []}}
        for run in RUNS:
            qos0_path = qos0_dir / f"{transport}_loss1pct_run{run}.json"
            if qos0_path.exists():
                with open(qos0_path) as f:
                    result = json.load(f)
                val = result["results"].get("spike_isolation_ratio")
                if val is not None:
                    data[transport]["qos0"]["spike_iso"].append(val)
                data[transport]["qos0"]["measured_rate"].append(
                    result["results"]["measured_rate"])

            qos1_path = qos1_dir / f"{transport}_qos1_run{run}.json"
            if qos1_path.exists():
                with open(qos1_path) as f:
                    result = json.load(f)
                val = result["results"].get("spike_isolation_ratio")
                if val is not None:
                    data[transport]["qos1"]["spike_iso"].append(val)
                data[transport]["qos1"]["measured_rate"].append(
                    result["results"]["measured_rate"])

    return data


def compute_ci(values, confidence=0.95):
    n = len(values)
    if n < 2:
        return np.mean(values), 0.0
    m = np.mean(values)
    sem = stats.sem(values)
    t_crit = stats.t.ppf((1 + confidence) / 2, df=n - 1)
    return m, t_crit * sem


def plot_grouped_bars(ax, data, metric, ylabel, title):
    num_transports = len(TRANSPORTS)
    num_qos = 2
    bar_width = 0.25
    group_positions = np.arange(num_transports)

    for qos_idx, qos_key in enumerate(["qos0", "qos1"]):
        means = []
        cis = []
        for transport in TRANSPORTS:
            values = data[transport][qos_key][metric]
            if values:
                m, ci_half = compute_ci(values)
                means.append(m)
                cis.append(ci_half)
            else:
                means.append(0)
                cis.append(0)

        offset = (qos_idx - (num_qos - 1) / 2) * bar_width
        positions = group_positions + offset

        ax.bar(
            positions,
            means,
            bar_width,
            yerr=cis,
            capsize=4,
            color=QOS_COLORS[qos_key],
            label=QOS_LABELS[qos_key],
            edgecolor="white",
            linewidth=0.5,
        )

    ax.set_xticks(group_positions)
    ax.set_xticklabels([TRANSPORT_LABELS[t] for t in TRANSPORTS])
    ax.set_ylabel(ylabel)
    ax.set_title(title)
    ax.legend(loc="best", framealpha=0.9)


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_qos_data(results_dir)
    if data is None:
        return

    fig, axes = plt.subplots(1, 2, figsize=(10, 4.5))

    plot_grouped_bars(axes[0], data, "spike_iso",
                      "Spike Isolation Ratio",
                      "HOL Blocking: QoS 0 vs QoS 1")

    plot_grouped_bars(axes[1], data, "measured_rate",
                      "Throughput (msgs/sec)",
                      "Throughput: QoS 0 vs QoS 1")

    fig.tight_layout()
    save_figure(fig, output_dir, "fig11_qos_comparison")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results_v2"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
