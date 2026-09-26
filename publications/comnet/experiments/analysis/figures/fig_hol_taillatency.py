import glob
import json
import statistics
import sys
from pathlib import Path

import matplotlib.pyplot as plt

sys.path.insert(0, str(Path(__file__).parent))
from style import (
    TRANSPORT_COLORS,
    TRANSPORT_LABELS,
    TRANSPORT_MARKERS,
    TRANSPORT_ORDER,
    apply_style,
    save_figure,
)

TOPICS = [2, 4, 8, 16, 32]
LOSS = 5
RESULTS = Path(__file__).resolve().parent.parent.parent / "results-v5"


def cell_files(transport, topics):
    if topics == 8:
        return glob.glob(str(RESULTS / "02_hol_blocking" / f"{transport}_loss{LOSS}pct_run*.json"))
    return glob.glob(str(RESULTS / "02c_topic_rate_sweep" / f"{transport}_t{topics}_r500_loss{LOSS}pct_run*.json"))


def worst_best(transport, topics):
    worst, best = [], []
    for f in cell_files(transport, topics):
        tp = json.load(open(f))["results"].get("topics", [])
        p99 = [x.get("p99_us", 0) / 1000 for x in tp if x.get("p99_us")]
        if p99:
            worst.append(max(p99))
            best.append(min(p99))
    if not worst:
        return None
    return statistics.median(worst), statistics.median(best)


def main(output_dir: Path):
    apply_style()
    fig, ax = plt.subplots(figsize=(7, 4.3))
    x = range(len(TOPICS))
    for tr in TRANSPORT_ORDER:
        w, b = [], []
        for t in TOPICS:
            r = worst_best(tr, t)
            w.append(r[0] if r else None)
            b.append(r[1] if r else None)
        ax.plot(
            x, w, TRANSPORT_MARKERS[tr] + "-", color=TRANSPORT_COLORS[tr],
            label=TRANSPORT_LABELS[tr], markersize=7, linewidth=1.6,
            markeredgecolor="white", markeredgewidth=0.7, zorder=3,
        )
        if tr == "quic-pertopic":
            ax.plot(x, b, TRANSPORT_MARKERS[tr] + "--", color=TRANSPORT_COLORS[tr],
                    markersize=6, linewidth=1.2, alpha=0.7, zorder=3)
            ax.fill_between(x, b, w, color=TRANSPORT_COLORS[tr], alpha=0.12, zorder=1)

    ax.set_xlabel("Topics")
    ax.set_ylabel("Per-topic p99 latency (ms)")
    ax.set_xticks(list(x))
    ax.set_xticklabels([str(t) for t in TOPICS])
    ax.set_ylim(bottom=0)
    ax.legend(loc="center left", framealpha=0.9)
    ax.text(0.98, 0.05, "solid: worst topic  |  dashed: best (per-topic)",
            transform=ax.transAxes, fontsize=7.5, style="italic", color="0.4", ha="right")
    fig.tight_layout()
    save_figure(fig, output_dir, "fig_hol_taillatency")


if __name__ == "__main__":
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent / "output"
    out.mkdir(parents=True, exist_ok=True)
    main(out)
