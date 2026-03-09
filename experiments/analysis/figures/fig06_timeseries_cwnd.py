import csv
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from style import apply_style, save_figure

TOPIC_COLORS = plt.cm.tab10.colors


def load_messages(messages_path: Path):
    topic_data = {}
    with open(messages_path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            topic_idx = int(row["topic_idx"])
            receive_ns = int(row["receive_ns"])
            latency_us = float(row["latency_us"])
            topic_data.setdefault(topic_idx, {"receive_ns": [], "latency_us": []})
            topic_data[topic_idx]["receive_ns"].append(receive_ns)
            topic_data[topic_idx]["latency_us"].append(latency_us)
    return topic_data


def load_quinn_stats(stats_path: Path):
    timestamps_ns = []
    cwnd_values = []
    with open(stats_path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            timestamps_ns.append(int(row["timestamp_ns"]))
            cwnd_values.append(int(row["cwnd"]))
    return np.array(timestamps_ns), np.array(cwnd_values)


def main(results_dir: Path, output_dir: Path):
    apply_style()

    exp02_dir = results_dir / "02_hol_blocking"
    messages_path = exp02_dir / "quic-pertopic_loss1pct_run1_messages.csv"
    quinn_path = exp02_dir / "quic-pertopic_loss1pct_run1_quinn_stats.csv"

    if not messages_path.exists():
        print(f"  WARNING: {messages_path} not found, skipping fig06")
        return
    if not quinn_path.exists():
        print(f"  WARNING: {quinn_path} not found, skipping fig06")
        return

    topic_data = load_messages(messages_path)
    quinn_ts_ns, cwnd = load_quinn_stats(quinn_path)

    all_timestamps = []
    for td in topic_data.values():
        all_timestamps.extend(td["receive_ns"])
    all_timestamps.extend(quinn_ts_ns.tolist())
    t0_ns = min(all_timestamps)

    fig, (ax_latency, ax_cwnd) = plt.subplots(
        2, 1, figsize=(10, 6), sharex=True, gridspec_kw={"height_ratios": [2, 1]}
    )

    for topic_idx in sorted(topic_data.keys()):
        td = topic_data[topic_idx]
        time_s = [(t - t0_ns) / 1e9 for t in td["receive_ns"]]
        latency_ms = [l / 1000.0 for l in td["latency_us"]]

        sorted_pairs = sorted(zip(time_s, latency_ms))
        time_sorted = [p[0] for p in sorted_pairs]
        latency_sorted = [p[1] for p in sorted_pairs]

        color_idx = topic_idx % len(TOPIC_COLORS)
        ax_latency.plot(
            time_sorted,
            latency_sorted,
            color=TOPIC_COLORS[color_idx],
            linewidth=0.4,
            alpha=0.7,
            label=f"Topic {topic_idx}",
        )

    ax_latency.set_ylabel("Latency (ms)")
    ax_latency.set_title("Per-Topic Latency and QUIC Congestion Window (1% Loss)")
    ax_latency.legend(
        loc="upper right", fontsize=7, ncol=4, framealpha=0.8, markerscale=0.8
    )

    quinn_time_s = (quinn_ts_ns - t0_ns) / 1e9
    ax_cwnd.plot(quinn_time_s, cwnd, color="#333333", linewidth=1.0)
    ax_cwnd.set_xlabel("Time (s)")
    ax_cwnd.set_ylabel("CWND (bytes)")
    ax_cwnd.fill_between(quinn_time_s, cwnd, alpha=0.1, color="#333333")

    fig.tight_layout()
    save_figure(fig, output_dir, "fig06_timeseries_cwnd")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results_v3"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
