import csv
import glob
import json
import statistics
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats as st

sys.path.insert(0, str(Path(__file__).parent))
from style import STRATEGY_COLORS, STRATEGY_LABELS, STRATEGY_MARKERS, apply_style, save_figure

TOPIC_COUNTS = [1, 2, 4, 8, 16]
STREAM_LIMITS = [25, 100, 250, 1000]
STREAM_WINDOWS = [131_072, 262_144, 1_048_576]
BYTES_PER_MESSAGE = 312.8


def publish_rates(base, label):
    rates = []
    for path in glob.glob(str(base / f"{label}_run*_pub.json")):
        try:
            results = json.load(open(path))["results"]
        except (json.JSONDecodeError, KeyError, OSError):
            continue
        if results.get("elapsed_secs") and results.get("published"):
            rates.append(results["published"] / results["elapsed_secs"])
    return rates


def mean_ci(values):
    if not values:
        return None, None
    mean = statistics.mean(values)
    if len(values) < 2:
        return mean, 0.0
    return mean, st.t.ppf(0.975, len(values) - 1) * st.sem(values)


def median_rtt(base, label):
    samples = []
    for path in glob.glob(str(base / f"{label}_run*_broker_quic_*.csv")):
        rows = list(csv.DictReader(open(path)))
        if rows:
            samples.append(statistics.median([int(r["rtt_us"]) for r in rows if r.get("rtt_us")]))
    return statistics.median(samples) / 1e6 if samples else None


def series(base, labels):
    means, errs = [], []
    for label in labels:
        mean, err = mean_ci(publish_rates(base, label))
        means.append(np.nan if mean is None else mean / 1000.0)
        errs.append(0.0 if err is None else err / 1000.0)
    return means, errs


def topic_sweep(base, output_dir):
    fig, ax = plt.subplots(1, 1, figsize=(4.5, 3.2))
    for strategy in ["control-only", "per-topic", "per-publish"]:
        labels = [f"{strategy}_t{t}_sdef_wdef_d25_l2_tput" for t in TOPIC_COUNTS]
        means, errs = series(base, labels)
        ax.errorbar(
            TOPIC_COUNTS, means, yerr=errs,
            marker=STRATEGY_MARKERS[strategy], color=STRATEGY_COLORS[strategy],
            label=STRATEGY_LABELS[strategy], linewidth=2, markersize=8, capsize=3,
        )
    ax.set_xscale("log", base=2)
    ax.set_xticks(TOPIC_COUNTS)
    ax.set_xticklabels([str(t) for t in TOPIC_COUNTS])
    ax.set_xlabel("Topics on the connection")
    ax.set_ylabel("Publish rate (K msg/s)")
    ax.set_ylim(0, 62)
    ax.set_yticks([0, 10, 20, 30, 40, 50, 60])
    ax.grid(True, axis="y", linewidth=0.4, alpha=0.4)
    ax.legend(loc="center", bbox_to_anchor=(0.5, 0.30), fontsize=8)
    fig.tight_layout()
    save_figure(fig, output_dir, "fig_topic_sweep")


def limit_sweep(base, output_dir):
    fig, (ax_credit, ax_window) = plt.subplots(2, 1, figsize=(4.5, 5.6))

    labels = [f"per-publish_t8_s{limit}_wdef_d25_l2_tput" for limit in STREAM_LIMITS]
    means, errs = series(base, labels)
    rtt = median_rtt(base, labels[1]) or 0.0252
    model = [limit / rtt / 1000.0 for limit in STREAM_LIMITS]
    ax_credit.plot(STREAM_LIMITS, model, linestyle="--", color="0.45", linewidth=1.5,
                   label="credit $\\div$ RTT")
    ax_credit.errorbar(STREAM_LIMITS, means, yerr=errs, marker=STRATEGY_MARKERS["per-publish"],
                       color=STRATEGY_COLORS["per-publish"], linewidth=2, markersize=8, capsize=3,
                       label=STRATEGY_LABELS["per-publish"])
    for strategy in ["control-only", "per-topic"]:
        flat, flat_errs = series(base, [f"{strategy}_t8_s{limit}_wdef_d25_l2_tput" for limit in [100, 1000]])
        ax_credit.errorbar([100, 1000], flat, yerr=flat_errs, marker=STRATEGY_MARKERS[strategy],
                           color=STRATEGY_COLORS[strategy], linewidth=2, markersize=8, capsize=3,
                           label=STRATEGY_LABELS[strategy])
    ax_credit.set_xscale("log")
    ax_credit.set_yscale("log")
    ax_credit.set_xticks(STREAM_LIMITS)
    ax_credit.set_xticklabels([str(limit) for limit in STREAM_LIMITS])
    ax_credit.set_yticks([1, 3, 10, 30, 60])
    ax_credit.set_yticklabels(["1", "3", "10", "30", "60"])
    ax_credit.set_xlabel("(a)  Concurrent stream limit")
    ax_credit.set_ylabel("Publish rate (K msg/s)")
    ax_credit.grid(True, axis="y", linewidth=0.4, alpha=0.4)
    ax_credit.legend(loc="lower right", fontsize=7)

    window_series = [
        ("control-only", 8, STRATEGY_COLORS["control-only"], STRATEGY_MARKERS["control-only"], "-", "Control-only"),
        ("per-topic", 1, STRATEGY_COLORS["per-topic"], STRATEGY_MARKERS["per-topic"], "--", "Per-topic, 1 topic"),
        ("per-topic", 8, STRATEGY_COLORS["per-topic"], "o", "-", "Per-topic, 8 topics"),
    ]
    x_kb = [w / 1024 for w in STREAM_WINDOWS]
    model_w = [w / rtt / BYTES_PER_MESSAGE / 1000.0 for w in STREAM_WINDOWS]
    ax_window.plot(x_kb, model_w, linestyle=":", color="0.45", linewidth=1.5,
                   label="window $\\div$ RTT")
    for strategy, topics, color, marker, linestyle, label in window_series:
        labels = [f"{strategy}_t{topics}_sdef_w{w}_d25_l2_tput" for w in STREAM_WINDOWS]
        means, errs = series(base, labels)
        ax_window.errorbar(x_kb, means, yerr=errs, marker=marker, color=color, linestyle=linestyle,
                           linewidth=2, markersize=8, capsize=3, label=label)
    ax_window.set_xscale("log", base=2)
    ax_window.set_xticks(x_kb)
    ax_window.set_xticklabels(["128", "256", "1024"])
    ax_window.set_ylim(0, 92)
    ax_window.set_yticks([0, 20, 40, 60, 80])
    ax_window.set_xlabel("(b)  Per-stream receive window (KB)")
    ax_window.set_ylabel("Publish rate (K msg/s)")
    ax_window.grid(True, axis="y", linewidth=0.4, alpha=0.4)
    ax_window.legend(loc="upper left", fontsize=7)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig_transport_limits")


import re

RATE_CEILING = 54_000.0
DEFAULT_WINDOW = 262_144
DEFAULT_STREAM_LIMIT = 100

CELL_PATTERN = re.compile(
    r"(?P<strategy>[a-z-]+)_t(?P<topics>\d+)_s(?P<streams>def|\d+)"
    r"_w(?P<window>def|\d+)_d(?P<delay>\d+)_l(?P<loss>\d+)_tput"
)


def predicted_rate(strategy, topics, stream_limit, window, rtt):
    if strategy == "per-publish":
        return min(stream_limit / rtt, RATE_CEILING)
    streams = 1 if strategy == "control-only" else topics
    return min(streams * window / rtt / BYTES_PER_MESSAGE, RATE_CEILING)


def model_collapse(base, output_dir):
    points = {}
    for pub in glob.glob(str(base / "*_tput_run1_pub.json")):
        label = Path(pub).name.replace("_run1_pub.json", "")
        match = CELL_PATTERN.match(label)
        if not match:
            continue
        cell = match.groupdict()
        rates = publish_rates(base, label)
        rtt = median_rtt(base, label)
        if not rates or not rtt:
            continue
        window = DEFAULT_WINDOW if cell["window"] == "def" else int(cell["window"])
        limit = DEFAULT_STREAM_LIMIT if cell["streams"] == "def" else int(cell["streams"])
        predicted = predicted_rate(cell["strategy"], int(cell["topics"]), limit, window, rtt)
        points.setdefault(cell["strategy"], []).append((predicted / 1000.0, statistics.mean(rates) / 1000.0))

    fig, ax = plt.subplots(1, 1, figsize=(4.5, 3.6))
    diagonal = np.array([0.7, 80.0])
    ax.fill_between(diagonal, diagonal * 0.9, diagonal * 1.1, color="0.85", zorder=0,
                    label="$\\pm$10%")
    ax.plot(diagonal, diagonal, color="0.35", linewidth=1.2, zorder=1)
    for strategy in ["control-only", "per-topic", "per-publish"]:
        if strategy not in points:
            continue
        xs, ys = zip(*points[strategy])
        ax.scatter(xs, ys, s=46, marker=STRATEGY_MARKERS[strategy], color=STRATEGY_COLORS[strategy],
                   edgecolor="white", linewidth=0.6, zorder=3, label=STRATEGY_LABELS[strategy])
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlim(0.7, 80)
    ax.set_ylim(0.7, 80)
    ticks = [1, 3, 10, 30, 60]
    ax.set_xticks(ticks); ax.set_xticklabels([str(t) for t in ticks])
    ax.set_yticks(ticks); ax.set_yticklabels([str(t) for t in ticks])
    ax.set_xlabel("Rate predicted by the binding limit (K msg/s)")
    ax.set_ylabel("Measured rate (K msg/s)")
    ax.grid(True, linewidth=0.4, alpha=0.4)
    ax.legend(loc="upper left", fontsize=8)
    fig.tight_layout()
    save_figure(fig, output_dir, "fig_model_collapse")


def main(results_dir, output_dir):
    apply_style()
    base = Path(results_dir) / "04_transport_limits"
    if not base.exists():
        print(f"  WARNING: {base} not found")
        return
    topic_sweep(base, output_dir)
    limit_sweep(base, output_dir)
    model_collapse(base, output_dir)


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    results = Path(sys.argv[1]) if len(sys.argv) > 1 else script_dir.parent.parent / "results-v5"
    output = Path(sys.argv[2]) if len(sys.argv) > 2 else script_dir / "output"
    output.mkdir(parents=True, exist_ok=True)
    main(results, output)
