import json
import statistics
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent / "results-v5"
D3 = ROOT / "03_throughput_under_loss"
D4 = ROOT / "04_stream_strategies"
LOSSES = [0, 1, 2, 5, 10]
TOPICS = [1, 4, 8, 16]
RUNS = range(1, 16)
ARMS3 = ["tcp", "tls", "quic-control-only", "quic-per-topic", "quic-per-publish"]
STRATS4 = ["control-only", "per-topic", "per-publish"]


def load(path: Path):
    try:
        if path.stat().st_size < 50:
            return None
        return json.load(open(path))
    except (OSError, ValueError):
        return None


def median_or_none(values):
    return round(statistics.median(values), 2) if values else None


def exp3(qos: int):
    print(f"=== Exp3 QoS{qos}: unique delivered (throughput_avg/subs) | fan-out recv/pub | ingest pub/s ===")
    print(f"  loss: {LOSSES}")
    for arm in ARMS3:
        uniq, fan, ing = [], [], []
        for loss in LOSSES:
            u, f, i = [], [], []
            for run in RUNS:
                sub = load(D3 / f"{arm}_qos{qos}_loss{loss}pct_run{run}.json")
                pub = load(D3 / f"{arm}_qos{qos}_loss{loss}pct_run{run}_pub.json")
                if sub:
                    subs = sub["config"].get("subscribers") or 1
                    u.append(sub["results"]["throughput_avg"] / subs)
                if sub and pub and pub["results"].get("published"):
                    f.append(sub["results"]["received"] / pub["results"]["published"])
                if pub and pub["results"].get("elapsed_secs"):
                    i.append(pub["results"]["published"] / pub["results"]["elapsed_secs"])
            uniq.append(median_or_none(u))
            fan.append(median_or_none(f))
            ing.append(median_or_none(i))
        n = sum(1 for run in RUNS if load(D3 / f"{arm}_qos{qos}_loss0pct_run{run}.json"))
        print(f"  {arm:18s} n={n:2d} uniq={uniq}")
        print(f"  {'':18s}      fanout={fan}")
        print(f"  {'':18s}      ingest={ing}")


def exp4():
    print("=== Exp4: publisher send rate (published/elapsed) | subscriber delivered (throughput_avg) ===")
    print(f"  topics: {TOPICS}")
    for strat in STRATS4:
        send, deliv = [], []
        for topics in TOPICS:
            s, d = [], []
            for run in RUNS:
                pub = load(D4 / f"{strat}_{topics}topics_throughput_run{run}_pub.json")
                sub = load(D4 / f"{strat}_{topics}topics_throughput_run{run}.json")
                if pub and pub["results"].get("elapsed_secs"):
                    s.append(pub["results"]["published"] / pub["results"]["elapsed_secs"])
                if sub:
                    d.append(sub["results"]["throughput_avg"])
            send.append(median_or_none(s))
            deliv.append(median_or_none(d))
        print(f"  {strat:12s} send={send}")
        print(f"  {'':12s} deliv={deliv}")


def main():
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    if which in ("all", "exp3"):
        exp3(1)
        print()
    if which in ("all", "exp4"):
        exp4()
    if which == "exp3qos0":
        exp3(0)


if __name__ == "__main__":
    main()
