import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from scipy import stats

sys.path.insert(0, str(Path(__file__).parent))
from style import (
    TRANSPORT_COLORS,
    TRANSPORT_LABELS,
    TRANSPORT_MARKERS,
    TRANSPORT_ORDER,
    apply_style,
    save_figure,
)

RUNS = range(1, 16)


def load_rtt_data(results_dir: Path):
    exp02_dir = results_dir / "02_hol_blocking"
    exp02b_dir = results_dir / "02b_hol_rtt_sweep"
    exp02d_dir = results_dir / "02d_hol_rtt_boundary"

    sources = {}

    if exp02_dir.exists():
        for transport in TRANSPORT_ORDER:
            for run in RUNS:
                filepath = exp02_dir / f"{transport}_loss1pct_run{run}.json"
                if filepath.exists() and filepath.stat().st_size > 10:
                    sources.setdefault(transport, {}).setdefault(25, [])
                    with open(filepath) as f:
                        result = json.load(f)
                    sources[transport][25].append(
                        result["results"]["windowed_correlation"]
                    )

    if exp02b_dir.exists():
        for transport in TRANSPORT_ORDER:
            for rtt_ms in [10, 50, 100]:
                for run in RUNS:
                    filepath = exp02b_dir / f"{transport}_rtt{rtt_ms}ms_run{run}.json"
                    if filepath.exists() and filepath.stat().st_size > 10:
                        sources.setdefault(transport, {}).setdefault(rtt_ms, [])
                        with open(filepath) as f:
                            result = json.load(f)
                        sources[transport][rtt_ms].append(
                            result["results"]["windowed_correlation"]
                        )

    if exp02d_dir.exists():
        for transport in TRANSPORT_ORDER:
            for rtt_ms in [15, 20]:
                for run in RUNS:
                    filepath = (
                        exp02d_dir / f"{transport}_rtt{rtt_ms}ms_run{run}.json"
                    )
                    if filepath.exists() and filepath.stat().st_size > 10:
                        sources.setdefault(transport, {}).setdefault(rtt_ms, [])
                        with open(filepath) as f:
                            result = json.load(f)
                        sources[transport][rtt_ms].append(
                            result["results"]["windowed_correlation"]
                        )

    if not sources:
        print("  WARNING: no RTT sweep data found, skipping fig02")
        return None

    return sources


def compute_ci(values, confidence=0.95):
    n = len(values)
    if n < 2:
        return np.mean(values), 0.0
    mean = np.mean(values)
    sem = stats.sem(values)
    t_crit = stats.t.ppf((1 + confidence) / 2, df=n - 1)
    return mean, t_crit * sem


def main(results_dir: Path, output_dir: Path):
    apply_style()
    data = load_rtt_data(results_dir)
    if data is None:
        return

    fig, ax = plt.subplots(figsize=(7, 4.5))

    for transport in TRANSPORT_ORDER:
        if transport not in data:
            continue

        rtt_values = sorted(data[transport].keys())
        means = []
        ci_lows = []
        ci_highs = []

        for rtt in rtt_values:
            mean, ci_half = compute_ci(data[transport][rtt])
            means.append(mean)
            ci_lows.append(mean - ci_half)
            ci_highs.append(mean + ci_half)

        ax.plot(
            rtt_values,
            means,
            marker=TRANSPORT_MARKERS[transport],
            color=TRANSPORT_COLORS[transport],
            label=TRANSPORT_LABELS[transport],
            linewidth=1.5,
            markersize=6,
        )
        ax.fill_between(
            rtt_values,
            ci_lows,
            ci_highs,
            color=TRANSPORT_COLORS[transport],
            alpha=0.15,
        )

    ax.set_xlabel("RTT (ms)")
    ax.set_ylabel("Windowed Correlation")
    ax.set_title("HOL Blocking: Windowed Correlation vs. RTT")
    ax.set_ylim(0, 1.12)
    ax.axhline(y=1.0, color="gray", linewidth=0.5, linestyle="--", zorder=1)
    ax.legend(loc="best", framealpha=0.9)

    fig.tight_layout()
    save_figure(fig, output_dir, "fig02_spike_iso_vs_rtt")


if __name__ == "__main__":
    script_dir = Path(__file__).resolve().parent
    default_results = script_dir.parent.parent / "results-v5"
    default_output = script_dir / "output"
    results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
    output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
    output_dir.mkdir(parents=True, exist_ok=True)
    main(results_dir, output_dir)
