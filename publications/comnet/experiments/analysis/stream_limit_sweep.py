import glob
import json
import statistics
import sys
from pathlib import Path

STRATEGIES = ["control-only", "per-topic", "per-publish"]
LIMITS = [100, 250, 1000]
RTT_S = 0.025


def load(pattern, field):
    values = []
    for path in glob.glob(pattern):
        try:
            results = json.load(open(path))["results"]
        except (json.JSONDecodeError, KeyError, OSError):
            continue
        value = results.get(field)
        if value:
            values.append(value)
    return values


def summarise(values, scale=1.0):
    if not values:
        return None
    mean = statistics.mean(values) / scale
    spread = (statistics.stdev(values) / statistics.mean(values) * 100) if len(values) > 1 else 0.0
    return mean, spread, len(values)


def main(results_dir):
    base = Path(results_dir) / "04b_stream_limit_sweep"
    if not base.exists():
        print(f"missing {base}")
        return

    print("Single-connection publish rate (K msg/s), 8 topics, 25 ms, 2% loss")
    print(f"{'strategy':14s} " + " ".join(f"{'limit ' + str(l):>18s}" for l in LIMITS))
    for strategy in STRATEGIES:
        cells = []
        for limit in LIMITS:
            rates = load(str(base / f"{strategy}_limit{limit}_throughput_run*_pub.json"), "offered_rate")
            stat = summarise(rates, 1000.0)
            cells.append(f"{stat[0]:8.2f}K ±{stat[1]:4.1f}% n={stat[2]}" if stat else f"{'-':>18s}")
        print(f"{strategy:14s} " + " ".join(f"{c:>18s}" for c in cells))

    print()
    print("per-publish only: concurrent streams in flight (rate x 25 ms) against the cap")
    cells = []
    for limit in LIMITS:
        rates = load(str(base / f"per-publish_limit{limit}_throughput_run*_pub.json"), "offered_rate")
        stat = summarise(rates)
        cells.append(f"{stat[0] * RTT_S:5.0f}/{limit}" if stat else f"{'-':>10s}")
    print("  " + " ".join(f"{c:>10s}" for c in cells))
    print("  (only per-publish opens one stream per message, so this ratio is meaningful only there)")

    print()
    print("Experiment 2 cell: delivered rate at 2000 msg/s offered, 8 topics, 5% loss")
    for strategy in STRATEGIES:
        cells = []
        for limit in LIMITS:
            rates = load(str(base / f"{strategy}_limit{limit}_hol_r2000_loss5pct_run*.json"), "measured_rate")
            stat = summarise(rates)
            cells.append(f"{stat[0]:8.1f} ±{stat[1]:4.1f}% n={stat[2]}" if stat else f"{'-':>18s}")
        print(f"  {strategy:14s} " + " ".join(f"{c:>18s}" for c in cells))


if __name__ == "__main__":
    default = Path(__file__).resolve().parent.parent / "results-v5"
    main(sys.argv[1] if len(sys.argv) > 1 else default)
