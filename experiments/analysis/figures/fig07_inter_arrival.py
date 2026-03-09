import csv
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from style import apply_style, save_figure


def load_and_compute_cross_topic_inter_arrivals(messages_path: Path):
    rows = []
    with open(messages_path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            rows.append(
                {
                    "topic_idx": int(row["topic_idx"]),
                    "receive_ns": int(row["receive_ns"]),
                }
            )

    rows.sort(key=lambda r: r["receive_ns"])

    cross_topic_deltas_us = []
    for i in range(1, len(rows)):
        if rows[i]["topic_idx"] != rows[i - 1]["topic_idx"]:
            delta_ns = rows[i]["receive_ns"] - rows[i - 1]["receive_ns"]
            cross_topic_deltas_us.append(delta_ns / 1000.0)

    return np.array(cross_topic_deltas_us)


def main(results_dir: Path, output_dir: Path):
    apply_style()

    exp02_dir = results_dir / "02_hol_blocking"
    messages_path = exp02_dir / "quic-pertopic_loss0pct_run1_messages.csv"

    if not messages_path.exists():
        print(f"  WARNING: {messages_path} not found, skipping fig07")
        return

    deltas_us = load_and_compute_cross_topic_inter_arrivals(messages_path)
    if len(deltas_us) == 0:
        print("  WARNING: no cross-topic inter-arrival data, skipping fig07")
        return

    deltas_positive = deltas_us[deltas_us > 0]

    fig, ax = plt.subplots(figsize=(7, 4.5))

    log_bins = np.logspace(
        np.log10(max(deltas_positive.min(), 0.01)),
        np.log10(deltas_positive.max()),
        80,
    )

    ax.hist(
        deltas_positive,
        bins=log_bins,
        color="#2ca02c",
        alpha=0.75,
        edgecolor="white",
        linewidth=0.3,
    )

    ax.set_xscale("log")
    ax.set_xlabel("Cross-Topic Inter-Arrival Time (us)")
    ax.set_ylabel("Count")
    ax.set_title(
        "Distribution of Cross-Topic Inter-Arrival Times\n"
        "(QUIC Per-Topic, 0% Loss)"
    )

    median_val = np.median(deltas_positive)
    ax.axvline(
        x=median_val,
        color="#d62728",
        linestyle="--",
        linewidth=1.0,
        label=f"Median: {median_val:.1f} us",
    )
    ax.legend(loc="upper right", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig07_inter_arrival")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results_v3"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
