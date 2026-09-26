import glob
import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from style import (
    TRANSPORT_COLORS,
    TRANSPORT_LABELS,
    TRANSPORT_MARKERS,
    TRANSPORT_ORDER,
    apply_style,
    save_figure,
)

LOSSES = [0, 1, 2, 5]
BASE = Path(__file__).resolve().parent.parent.parent / "results-v5" / "02_hol_blocking"


def run_avgp99(transport, loss):
    vals = []
    for f in glob.glob(str(BASE / f"{transport}_loss{loss}pct_run*.json")):
        tp = json.load(open(f))["results"].get("topics", [])
        p = [x.get("p99_us", 0) / 1000 for x in tp if x.get("p99_us")]
        if p:
            vals.append(sum(p) / len(p))
    return vals


def main(output_dir: Path):
    apply_style()
    fig, ax = plt.subplots(figsize=(7, 4.3))
    x = range(len(LOSSES))
    for tr in TRANSPORT_ORDER:
        med, lo, hi = [], [], []
        for loss in LOSSES:
            v = run_avgp99(tr, loss)
            if v:
                med.append(float(np.median(v)))
                lo.append(float(np.percentile(v, 10)))
                hi.append(float(np.percentile(v, 90)))
            else:
                med.append(np.nan)
                lo.append(np.nan)
                hi.append(np.nan)
        ax.plot(x, med, TRANSPORT_MARKERS[tr] + "-", color=TRANSPORT_COLORS[tr],
                label=TRANSPORT_LABELS[tr], markersize=7, linewidth=1.6,
                markeredgecolor="white", markeredgewidth=0.7, zorder=3)
        ax.fill_between(x, lo, hi, color=TRANSPORT_COLORS[tr], alpha=0.13, zorder=1)

    ax.set_xlabel("Packet loss rate")
    ax.set_ylabel("Mean per-topic p99 latency (ms)")
    ax.set_xticks(list(x))
    ax.set_xticklabels([f"{l}%" for l in LOSSES])
    ax.set_ylim(bottom=0)
    ax.legend(loc="upper left", framealpha=0.9)
    fig.tight_layout()
    save_figure(fig, output_dir, "fig_hol_tail_vs_loss")


if __name__ == "__main__":
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent / "output"
    out.mkdir(parents=True, exist_ok=True)
    main(out)
