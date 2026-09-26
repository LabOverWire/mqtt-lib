import json
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
RATES = [125, 250, 500, 1000, 2000]


def main(decomp_path: Path, output_dir: Path):
    apply_style()
    with open(decomp_path) as f:
        d = json.load(f)["1"]

    fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(5, 6.4))

    for tr in TRANSPORT_ORDER:
        cells = d["topics"].get(tr, {})
        xs, ys, es = [], [], []
        for t in TOPICS:
            c = cells.get(str(t))
            if c:
                xs.append(t)
                ys.append(c["excess"][0])
                es.append(c["excess"][1])
        ax1.errorbar(
            range(len(xs)), ys, yerr=es,
            fmt=TRANSPORT_MARKERS[tr] + "-", color=TRANSPORT_COLORS[tr],
            label=TRANSPORT_LABELS[tr], markersize=7, capsize=3, linewidth=1.5,
            markeredgecolor="white", markeredgewidth=0.7, zorder=3,
        )
    ax1.set_xlabel("Topics (streams)")
    ax1.set_ylabel("Excess co-occurrence (real coupling)")
    ax1.set_xticks(range(len(TOPICS)))
    ax1.set_xticklabels([str(t) for t in TOPICS])
    ax1.set_ylim(0, 1.0)
    ax1.text(0.03, 0.06, "more isolated", transform=ax1.transAxes, fontsize=8, style="italic", color="0.4")
    ax1.legend(loc="lower right", framealpha=0.9, fontsize=8)
    ax1.set_title("(a) Isolation vs. topic count (rate 500)", fontsize=9)

    for tr in TRANSPORT_ORDER:
        cells = d["rate"].get(tr, {})
        xs, ys, es = [], [], []
        for r in RATES:
            c = cells.get(str(r))
            if c:
                xs.append(r)
                ys.append(c["null"][0])
                es.append(c["null"][1])
        ax2.errorbar(
            range(len(xs)), ys, yerr=es,
            fmt=TRANSPORT_MARKERS[tr] + "-", color=TRANSPORT_COLORS[tr],
            label=TRANSPORT_LABELS[tr], markersize=7, capsize=3, linewidth=1.5,
            markeredgecolor="white", markeredgewidth=0.7, zorder=3,
        )
    ax2.set_xlabel("Offered rate (msg/s, 8 topics)")
    ax2.set_ylabel("Null co-occurrence (density artifact)")
    ax2.set_xticks(range(len(RATES)))
    ax2.set_xticklabels([str(r) for r in RATES])
    ax2.set_ylim(0, 0.6)
    ax2.set_title("(b) Density confound vs. rate", fontsize=9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig_hol_excess")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_decomp = script_dir.parent.parent / "results-v5" / "02c_topic_rate_sweep" / "decomposition.json"
    default_output = script_dir / "output"
    decomp = Path(sys.argv[1]) if len(sys.argv) > 1 else default_decomp
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(decomp, output_dir)
