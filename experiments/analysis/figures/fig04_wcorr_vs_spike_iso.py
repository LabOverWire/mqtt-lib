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

EXP_DIRS = [
    "02_hol_blocking",
    "02b_hol_rtt_sweep",
    "02c_hol_topic_scaling",
    "02d_hol_rtt_boundary",
    "02e_hol_qos1",
]


def load_all_runs(results_dir: Path):
    scatter_data = {t: {"wcorr": [], "spike_iso": []} for t in TRANSPORT_ORDER}

    for exp_name in EXP_DIRS:
        exp_dir = results_dir / exp_name
        if not exp_dir.exists():
            continue

        for filepath in sorted(exp_dir.glob("*.json")):
            stem = filepath.stem
            transport = None
            for t in TRANSPORT_ORDER:
                if stem.startswith(t + "_"):
                    transport = t
                    break
            if transport is None:
                continue

            with open(filepath) as f:
                result = json.load(f)

            results_block = result.get("results", {})
            wcorr = results_block.get("windowed_correlation")
            spike_iso = results_block.get("spike_isolation_ratio")

            if wcorr is not None and spike_iso is not None:
                scatter_data[transport]["wcorr"].append(wcorr)
                scatter_data[transport]["spike_iso"].append(spike_iso)

    total_points = sum(
        len(v["wcorr"]) for v in scatter_data.values()
    )
    if total_points == 0:
        print("  WARNING: no scatter data found, skipping fig04")
        return None

    return scatter_data


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_all_runs(results_dir)
    if data is None:
        return

    fig, ax = plt.subplots(figsize=(7, 5.5))

    for transport in TRANSPORT_ORDER:
        wcorr_vals = data[transport]["wcorr"]
        spike_vals = data[transport]["spike_iso"]
        if not wcorr_vals:
            continue
        ax.scatter(
            wcorr_vals,
            spike_vals,
            color=TRANSPORT_COLORS[transport],
            marker=TRANSPORT_MARKERS[transport],
            label=TRANSPORT_LABELS[transport],
            s=30,
            alpha=0.6,
            edgecolors="none",
        )

    ax.axvline(x=0.5, color="gray", linestyle="--", linewidth=0.8, alpha=0.6)
    ax.axhline(y=0.5, color="gray", linestyle="--", linewidth=0.8, alpha=0.6)

    ax.annotate(
        "Divergent\nregion",
        xy=(0.85, 0.15),
        fontsize=9,
        fontstyle="italic",
        color="gray",
        ha="center",
        va="center",
        bbox=dict(boxstyle="round,pad=0.3", facecolor="lightyellow", alpha=0.7),
    )

    ax.set_xlabel("Windowed Correlation")
    ax.set_ylabel("Spike Isolation Ratio")
    ax.set_title("Windowed Correlation vs. Spike Isolation (All Experiments)")
    ax.set_xlim(-0.1, 1.1)
    ax.set_ylim(-0.05, 1.05)
    ax.legend(loc="upper left", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig04_wcorr_vs_spike_iso")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
