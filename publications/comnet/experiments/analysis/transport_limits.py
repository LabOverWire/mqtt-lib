import csv
import glob
import json
import statistics
import sys
from pathlib import Path

from scipy import stats as st

STRATEGIES = ["control-only", "per-topic", "per-publish"]
DEFAULT_STREAM_WINDOW = 262_144
DEFAULT_MAX_STREAMS = 100


def cell_paths(base, label):
    return sorted(p for p in glob.glob(str(base / f"{label}_run*_pub.json")))


def publish_rates(base, label):
    rates = []
    for path in cell_paths(base, label):
        try:
            results = json.load(open(path))["results"]
        except (json.JSONDecodeError, KeyError, OSError):
            continue
        elapsed = results.get("elapsed_secs")
        published = results.get("published")
        if elapsed and published:
            rates.append(published / elapsed)
    return rates


def hol_rates(base, label):
    rates = []
    for path in sorted(glob.glob(str(base / f"{label}_run*.json"))):
        if path.endswith("_pub.json"):
            continue
        try:
            results = json.load(open(path))["results"]
        except (json.JSONDecodeError, KeyError, OSError):
            continue
        if results.get("measured_rate"):
            rates.append(results["measured_rate"])
    return rates


def summarise(values):
    if not values:
        return None
    mean = statistics.mean(values)
    if len(values) < 2:
        return mean, 0.0, 1
    half = st.t.ppf(0.975, len(values) - 1) * st.sem(values)
    return mean, half, len(values)


def fmt(stat, scale=1000.0):
    if stat is None:
        return f"{'-':>20s}"
    return f"{stat[0] / scale:8.2f} ±{stat[1] / scale:5.2f} n={stat[2]}"


def broker_stats(base, label):
    rtts, blocked_stream, blocked_conn, blocked_streams_uni = [], [], [], []
    for path in sorted(glob.glob(str(base / f"{label}_run*_broker_quic_*.csv"))):
        rows = list(csv.DictReader(open(path)))
        if not rows:
            continue
        last = rows[-1]
        rtt_values = [int(r["rtt_us"]) for r in rows if r.get("rtt_us")]
        if rtt_values:
            rtts.append(statistics.median(rtt_values))
        blocked_stream.append(int(last.get("stream_data_blocked", 0)))
        blocked_conn.append(int(last.get("data_blocked", 0)))
        blocked_streams_uni.append(int(last.get("streams_blocked_uni", 0)))
    if not rtts:
        return None
    return {
        "rtt_ms": statistics.median(rtts) / 1000.0,
        "stream_data_blocked": max(blocked_stream),
        "data_blocked": max(blocked_conn),
        "streams_blocked_uni": max(blocked_streams_uni),
    }


def in_flight(base, label):
    rates = publish_rates(base, label)
    info = broker_stats(base, label)
    if not rates or not info:
        return None
    return statistics.mean(rates) * info["rtt_ms"] / 1000.0


def part_a(base):
    print("PART A  publish rate (K msg/s) by topic count, default transport config, 25 ms, 2% loss")
    topics = [1, 2, 4, 8, 16]
    print(f"{'strategy':14s}" + "".join(f"{'t=' + str(t):>21s}" for t in topics))
    for strategy in STRATEGIES:
        cells = [fmt(summarise(publish_rates(base, f"{strategy}_t{t}_sdef_wdef_d25_l2_tput"))) for t in topics]
        print(f"{strategy:14s}" + "".join(f"{c:>21s}" for c in cells))


def part_b(base):
    print("\nPART B  stream credit, 8 topics, 25 ms, 2% loss")
    print(f"{'cell':34s} {'K msg/s':>20s} {'rtt ms':>7s} {'streams in flight':>12s} {'of cap':>10s}")
    for limit in [25, 100, 250, 1000]:
        label = f"per-publish_t8_s{limit}_wdef_d25_l2_tput"
        stat = summarise(publish_rates(base, label))
        info = broker_stats(base, label)
        flight = in_flight(base, label)
        rtt = f"{info['rtt_ms']:7.1f}" if info else f"{'-':>7s}"
        pct = 100.0 * flight / limit if flight else float("nan")
        print(f"{'per-publish limit ' + str(limit):34s} {fmt(stat):>20s} {rtt} {flight:12.1f} {pct:9.1f}%")
    for strategy in ["control-only", "per-topic"]:
        for limit in [100, 1000]:
            label = f"{strategy}_t8_s{limit}_wdef_d25_l2_tput"
            print(f"{strategy + ' limit ' + str(limit):34s} {fmt(summarise(publish_rates(base, label))):>20s}")

    print("\nPART B  delay sweep (a credit ceiling scales with 1/RTT)")
    for strategy in ["per-publish", "control-only"]:
        for delay in [10, 25, 50]:
            label = f"{strategy}_t8_s100_wdef_d{delay}_l2_tput"
            info = broker_stats(base, label)
            rtt = f"{info['rtt_ms']:7.1f}" if info else f"{'-':>7s}"
            print(f"{strategy + ' delay ' + str(delay) + 'ms':34s} {fmt(summarise(publish_rates(base, label))):>20s} {rtt}")

    print("\nPART B  Experiment 2 cell: delivered rate at 2000 msg/s offered, 8 topics, 5% loss")
    for limit in [100, 1000]:
        label = f"per-publish_t8_s{limit}_wdef_d25_l5_hol_r2000"
        print(f"{'per-publish limit ' + str(limit):34s} {fmt(summarise(hol_rates(base, label)), 1.0):>20s} msg/s")


def part_c(base):
    print("\nPART C  per-stream receive window, 25 ms, 2% loss")
    print(f"{'cell':34s} {'K msg/s':>20s} {'rtt ms':>7s} {'implied B/msg':>15s}")
    print("  (implied B/msg is constant only where the per-stream window is the binding limit)")
    for window in [131_072, 262_144, 1_048_576]:
        for strategy, topics in [("control-only", 8), ("per-topic", 1), ("per-topic", 8)]:
            label = f"{strategy}_t{topics}_sdef_w{window}_d25_l2_tput"
            stat = summarise(publish_rates(base, label))
            info = broker_stats(base, label)
            streams = topics if strategy == "per-topic" else 1
            flight = in_flight(base, label)
            rtt = f"{info['rtt_ms']:7.1f}" if info else f"{'-':>7s}"
            implied = window * streams / flight if flight else float("nan")
            name = f"{strategy} t{topics} w{window // 1024}K"
            print(f"{name:34s} {fmt(stat):>20s} {rtt} {implied:15.1f}")

    print("\nPART C  0% loss (separates congestion response from a fixed ceiling)")
    for strategy, window in [("control-only", 262_144), ("per-topic", 262_144), ("control-only", 1_048_576)]:
        label = f"{strategy}_t8_sdef_w{window}_d25_l0_tput"
        info = broker_stats(base, label)
        rtt = f"{info['rtt_ms']:7.1f}" if info else f"{'-':>7s}"
        print(f"{strategy + ' w' + str(window // 1024) + 'K loss 0%':34s} {fmt(summarise(publish_rates(base, label))):>20s} {rtt}")


def main(results_dir):
    base = Path(results_dir) / "04_transport_limits"
    if not base.exists():
        print(f"missing {base}")
        return
    part_a(base)
    part_b(base)
    part_c(base)


if __name__ == "__main__":
    default = Path(__file__).resolve().parent.parent / "results-v5"
    main(sys.argv[1] if len(sys.argv) > 1 else default)
