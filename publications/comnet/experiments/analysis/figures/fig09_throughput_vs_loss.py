import csv
import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
from matplotlib.ticker import FixedLocator, FuncFormatter, NullLocator
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

LOSS_RATES = [0, 1, 2, 5, 10]
LOSS_LABELS = ["0%", "1%", "2%", "5%", "10%"]
RUNS = range(1, 16)
Y_TICKS = [500, 1000, 2000, 5000, 10000, 20000, 50000]
GROUPS = (1, 2, 3)
PER_PACKET_MODE = "router"
PER_PACKET_PHASE = "main"
PER_PACKET_CONFIGS = {
    "tcp": "tcp",
    "tls": "tls",
    "quic-control-only": "quic-main",
    "quic-per-topic": "quic-main-ptopic",
    "quic-per-publish": "quic-main-ppub",
}


def load_per_buffer(results_dir: Path):
    exp_dir = results_dir / "03_throughput_under_loss"
    data = {}
    for strategy in THROUGHPUT_ORDER:
        for loss in LOSS_RATES:
            values = []
            for run in RUNS:
                filepath = exp_dir / f"{strategy}_qos0_loss{loss}pct_run{run}.json"
                if not filepath.exists():
                    continue
                result = json.load(open(filepath))
                subscribers = result["config"].get("subscribers") or 1
                values.append(result["results"]["throughput_avg"] / subscribers)
            if values:
                data[(strategy, loss)] = values
    return data


def load_per_packet(results_dir: Path):
    strategy_of = {config: strategy for strategy, config in PER_PACKET_CONFIGS.items()}
    data = {}
    for group in GROUPS:
        exp_dir = results_dir / f"03e_single_segment_g{group}"
        accepted = {row["label"] for row in csv.DictReader(open(exp_dir / "acceptance.csv")) if row["pass"] == "True"}
        for row in csv.DictReader(open(exp_dir / "manifest.csv")):
            if row["phase"] != PER_PACKET_PHASE or row["mode"] != PER_PACKET_MODE or row["broker_probe"] == "1":
                continue
            strategy = strategy_of.get(row["config"])
            if strategy is None or row["label"] not in accepted:
                continue
            result = json.load(open(exp_dir / f"{row['label']}.json"))
            subscribers = result["config"].get("subscribers") or 1
            data.setdefault((strategy, int(row["loss"])), []).append(result["results"]["throughput_avg"] / subscribers)
    return data


def compute_ci(values, confidence=0.95):
    mean = np.mean(values)
    if len(values) < 2:
        return mean, 0.0
    return mean, stats.t.ppf((1 + confidence) / 2, df=len(values) - 1) * stats.sem(values)


def series(data, strategy):
    points = [(loss, *compute_ci(data[(strategy, loss)])) for loss in LOSS_RATES if (strategy, loss) in data]
    return [np.array(column) for column in zip(*points)] if points else None


def main(results_dir: Path, output_dir: Path):
    apply_style()
    per_buffer = load_per_buffer(results_dir)
    per_packet = load_per_packet(results_dir)

    fig, ax = plt.subplots(figsize=(7, 4))
    for strategy in THROUGHPUT_ORDER:
        color = THROUGHPUT_COLORS[strategy]
        reference = series(per_buffer, strategy)
        if reference is not None:
            x, mean, _ = reference
            ax.plot(x, mean, linestyle="--", color=color, alpha=0.35, linewidth=1.0)
        current = series(per_packet, strategy)
        if current is None:
            continue
        x, mean, half = current
        ax.errorbar(
            x,
            mean,
            yerr=half,
            marker=THROUGHPUT_MARKERS[strategy],
            color=color,
            label=THROUGHPUT_LABELS[strategy],
            linewidth=1.5,
            markersize=5,
            capsize=3,
        )
        for loss in x:
            values = per_packet[(strategy, int(loss))]
            print(f"{strategy} {int(loss)}%: n={len(values)} mean={np.mean(values):.0f}")

    ax.plot([], [], linestyle="--", color="grey", alpha=0.5, label="Per-buffer loss on broker egress")
    ax.set_yscale("log")
    ax.yaxis.set_major_locator(FixedLocator(Y_TICKS))
    ax.yaxis.set_minor_locator(NullLocator())
    ax.yaxis.set_major_formatter(FuncFormatter(lambda value, _: f"{value / 1000:g}K"))
    ax.set_ylim(Y_TICKS[0], 80000)
    ax.set_ylabel("Delivered throughput (unique msg/s)")
    ax.set_xlabel("Packet Loss Rate (%)")
    ax.set_xticks(LOSS_RATES)
    ax.set_xticklabels(LOSS_LABELS)
    ax.legend(loc="best", framealpha=0.9)
    ax.set_title("Delivered Throughput vs. Packet Loss Rate (QoS 0)")

    fig.tight_layout()
    save_figure(fig, output_dir, "fig09_throughput_vs_loss")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results-v5"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
