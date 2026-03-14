import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats

sys.path.insert(0, str(Path(__file__).parent))
from style import (
    DATAGRAM_COLORS,
    DATAGRAM_LABELS,
    DATAGRAM_ORDER,
    apply_style,
    save_figure,
)

DELAY = 50
LOSS_RATES = [0, 1, 5, 10]
LOSS_LABELS = ["0%", "1%", "5%", "10%"]
RUNS = range(1, 16)


def compute_ci(values, confidence=0.95):
    n = len(values)
    if n < 2:
        return np.mean(values), 0.0
    m = np.mean(values)
    sem = stats.sem(values)
    t_crit = stats.t.ppf((1 + confidence) / 2, df=n - 1)
    return m, t_crit * sem


def load_data(results_dir: Path):
    exp_dir = results_dir / "05_datagram_vs_stream"
    if not exp_dir.exists():
        print(f"  WARNING: {exp_dir} not found, skipping fig12")
        return None

    data = {}
    for transport in DATAGRAM_ORDER:
        suffix = "datagram" if "datagram" in transport else "stream"
        data[transport] = {"latency": {}, "throughput": {}}
        for loss in LOSS_RATES:
            lat_values = []
            lat_p95 = []
            tp_values = []
            for run in RUNS:
                lat_path = exp_dir / f"quic-{suffix}_delay{DELAY}ms_loss{loss}pct_latency_run{run}.json"
                if lat_path.exists():
                    with open(lat_path) as f:
                        d = json.load(f)
                    lat_values.append(d["results"]["p50_us"])
                    lat_p95.append(d["results"]["p95_us"])

                tp_path = exp_dir / f"quic-{suffix}_delay{DELAY}ms_loss{loss}pct_throughput_run{run}.json"
                if tp_path.exists():
                    with open(tp_path) as f:
                        d = json.load(f)
                    tp_values.append(d["results"]["throughput_avg"])

            if lat_values:
                data[transport]["latency"][loss] = {"p50": lat_values, "p95": lat_p95}
            if tp_values:
                data[transport]["throughput"][loss] = tp_values
    return data


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_data(results_dir)
    if data is None:
        return

    fig, axes = plt.subplots(1, 2, figsize=(7, 3.5))
    bar_width = 0.3
    x = np.arange(len(LOSS_RATES))

    ax_lat = axes[0]
    for idx, transport in enumerate(DATAGRAM_ORDER):
        p50_means = []
        p50_errs = []
        p95_means = []
        p95_errs = []
        for loss in LOSS_RATES:
            lat_data = data[transport]["latency"].get(loss)
            if lat_data:
                m50, e50 = compute_ci(lat_data["p50"])
                m95, e95 = compute_ci(lat_data["p95"])
                p50_means.append(m50 / 1000.0)
                p50_errs.append(e50 / 1000.0)
                p95_means.append(m95 / 1000.0)
                p95_errs.append(e95 / 1000.0)
            else:
                p50_means.append(0)
                p50_errs.append(0)
                p95_means.append(0)
                p95_errs.append(0)

        offset = (idx - 0.5) * bar_width
        color = DATAGRAM_COLORS[transport]
        label = DATAGRAM_LABELS[transport]

        p95_extensions = [max(0, p95 - p50) for p50, p95 in zip(p50_means, p95_means)]
        yerr_asym = [p50_errs, p95_extensions]

        ax_lat.bar(
            x + offset, p50_means, bar_width, yerr=yerr_asym,
            color=color, alpha=0.85, label=f"{label}",
            capsize=3, edgecolor="white", linewidth=0.5,
        )

    ax_lat.set_xlabel("Packet Loss Rate")
    ax_lat.set_ylabel("Latency (ms)")
    ax_lat.set_title("(a) Latency at 50ms RTT")
    ax_lat.set_xticks(x)
    ax_lat.set_xticklabels(LOSS_LABELS)
    ax_lat.legend(loc="upper left", fontsize=8)

    ax_tp = axes[1]
    for idx, transport in enumerate(DATAGRAM_ORDER):
        means = []
        errs = []
        for loss in LOSS_RATES:
            tp_data = data[transport]["throughput"].get(loss)
            if tp_data:
                m, e = compute_ci(tp_data)
                means.append(m)
                errs.append(e)
            else:
                means.append(0)
                errs.append(0)

        offset = (idx - 0.5) * bar_width
        ax_tp.bar(
            x + offset, means, bar_width, yerr=errs,
            color=DATAGRAM_COLORS[transport], alpha=0.85,
            label=DATAGRAM_LABELS[transport],
            capsize=3, edgecolor="white", linewidth=0.5,
        )

    ax_tp.set_xlabel("Packet Loss Rate")
    ax_tp.set_ylabel("Throughput (msgs/sec)")
    ax_tp.set_title("(b) Throughput at 50ms RTT")
    ax_tp.set_xticks(x)
    ax_tp.set_xticklabels(LOSS_LABELS)
    ax_tp.legend(loc="best", fontsize=8)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig12_datagram_vs_stream")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results-v5"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
