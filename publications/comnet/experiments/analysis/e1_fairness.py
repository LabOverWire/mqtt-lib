import csv
import glob
import json
import statistics
import sys
from pathlib import Path


def broker_tx_mbit(broker_csv: Path):
    rows = [r for r in csv.DictReader(open(broker_csv)) if r["net_tx_bytes"].isdigit()]
    if len(rows) < 4:
        return None
    rows = rows[1:-1]
    dt = float(rows[-1]["timestamp"]) - float(rows[0]["timestamp"])
    db = int(rows[-1]["net_tx_bytes"]) - int(rows[0]["net_tx_bytes"])
    return db * 8 / 1e6 / dt if dt > 0 else None


def iperf_mbit(iperf_json: Path):
    end = json.load(open(iperf_json)).get("end", {})
    if "sum_received" not in end:
        return None
    return end["sum_received"]["bits_per_second"] / 1e6


def arm_fairness(results_dir: Path, arm: str, loss: int):
    prefix = f"{arm}_rate*_loss{loss}pct"
    shares, iperfs, jains = [], [], []
    for bc in sorted(glob.glob(str(results_dir / f"{prefix}_run*_broker_resources.csv"))):
        run = Path(bc).name.replace("_broker_resources.csv", "")
        ic = results_dir / f"{run}_iperf.json"
        mc = results_dir / f"{run}_messages.csv"
        tx = broker_tx_mbit(Path(bc))
        ip = iperf_mbit(ic) if ic.exists() else None
        if tx and ip and tx > ip:
            shares.append(100 * (tx - ip) / tx)
            iperfs.append(ip)
        if mc.exists():
            gp = per_connection_goodput(mc)
            if len(gp) > 1:
                jains.append(jain_index(list(gp.values())))
    med = lambda v: round(statistics.median(v), 1) if v else None
    return {"n": len(shares), "mqtt_wire_share_pct": med(shares),
            "iperf_mbit": med(iperfs), "jain": (round(statistics.median(jains), 3) if jains else None)}


def analyze_dir(results_dir: Path):
    arms = ["tcp-1conn", "tcp-Nconn", "quic-control", "quic-pertopic"]
    print(f"E1 fairness (MQTT share of a bottleneck shared with one greedy TCP flow)")
    print(f"{'arm':>14} {'loss':>5} | {'MQTT share%':>11} {'iperf Mbit':>11} {'intra-Jain':>11} {'n':>3}")
    for arm in arms:
        for loss in [0, 1]:
            r = arm_fairness(results_dir, arm, loss)
            if r["n"]:
                print(f"{arm:>14} {loss:>4}% | {str(r['mqtt_wire_share_pct']):>11} "
                      f"{str(r['iperf_mbit']):>11} {str(r['jain']):>11} {r['n']:>3}")


def per_connection_goodput(messages_csv: Path, trim: float = 0.1):
    receive_ns = {}
    for row in csv.DictReader(open(messages_csv)):
        conn = int(row["conn_idx"])
        receive_ns.setdefault(conn, []).append(int(row["receive_ns"]))

    all_ns = [ns for series in receive_ns.values() for ns in series]
    if not all_ns:
        return {}
    lo, hi = min(all_ns), max(all_ns)
    span = hi - lo
    start = lo + int(span * trim)
    end = hi - int(span * trim)
    window_s = max((end - start) / 1e9, 1e-9)

    goodput = {}
    for conn, series in receive_ns.items():
        in_window = sum(1 for ns in series if start <= ns <= end)
        goodput[conn] = in_window / window_s
    return goodput


def jain_index(values):
    if not values:
        return 0.0
    n = len(values)
    total = sum(values)
    total_sq = sum(v * v for v in values)
    if total_sq == 0:
        return 0.0
    return (total * total) / (n * total_sq)


def main(messages_csv: Path):
    goodput = per_connection_goodput(messages_csv)
    if not goodput:
        print(f"no data in {messages_csv}")
        return
    flows = [goodput[c] for c in sorted(goodput)]
    print(f"file: {messages_csv}")
    print(f"connections: {len(flows)}")
    for conn in sorted(goodput):
        print(f"  conn {conn}: {goodput[conn]:.1f} msg/s")
    print(f"aggregate: {sum(flows):.1f} msg/s")
    print(f"Jain fairness index: {jain_index(flows):.4f}  (1.0 = perfectly fair)")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(f"usage: {sys.argv[0]} <messages.csv | E1_fairness_dir>")
        sys.exit(1)
    target = Path(sys.argv[1])
    if target.is_dir():
        analyze_dir(target)
    else:
        main(target)
