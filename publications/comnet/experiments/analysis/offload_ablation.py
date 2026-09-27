import csv
import glob
import json
import re
import statistics
import sys
from pathlib import Path

ARMS = {"tls": "TCP+TLS 1.3", "quic-control": "QUIC control-only"}
LOSSES = [0, 1, 2, 5, 10]
OFFLOAD = ["on", "off"]


def rates(base, arm, offload, loss):
    out = []
    for g in (1, 2, 3):
        pat = base / f"03c_offload_ablation_g{g}" / f"{arm}_offload-{offload}_loss{loss}pct_run*.json"
        for f in glob.glob(str(pat)):
            if f.endswith("_pub.json"):
                continue
            try:
                r = json.load(open(f))["results"]
            except (json.JSONDecodeError, KeyError, OSError):
                continue
            if r.get("throughput_avg"):
                out.append(r["throughput_avg"])
    return out


def broker_cpu(base, arm, offload, loss):
    vals = []
    for g in (1, 2, 3):
        pat = base / f"03c_offload_ablation_g{g}" / f"{arm}_offload-{offload}_loss{loss}pct_run*_broker_resources.csv"
        for f in glob.glob(str(pat)):
            rows = list(csv.DictReader(open(f)))
            v = [float(r["cpu_percent"]) for r in rows if r.get("cpu_percent")]
            if v:
                vals.append(statistics.median(v))
    return statistics.median(vals) if vals else None


def qdisc_bpp(base, arm, offload, loss):
    ratios = []
    for g in (1, 2, 3):
        d = base / f"03c_offload_ablation_g{g}"
        for after in glob.glob(str(d / f"{arm}_offload-{offload}_loss{loss}pct_run*_qdisc_after.txt")):
            before = after.replace("_after.txt", "_before.txt")
            try:
                b = _sent(open(before).read())
                a = _sent(open(after).read())
            except OSError:
                continue
            if a and b and a[1] > b[1]:
                dbytes, dpkts = a[0] - b[0], a[1] - b[1]
                if dpkts > 0:
                    ratios.append(dbytes / dpkts)
    return statistics.median(ratios) if ratios else None


def _sent(text):
    m = re.search(r"Sent (\d+) bytes (\d+) pkt", text)
    return (int(m.group(1)), int(m.group(2))) if m else None


def main(results_dir):
    base = Path(results_dir)
    print("Delivered throughput (K msg/s aggregate), n runs pooled over 3 groups\n")
    header = f"{'arm':20s} {'offload':>7s} " + " ".join(f"{'loss' + str(l):>13s}" for l in LOSSES)
    print(header)
    table = {}
    for arm in ARMS:
        for off in OFFLOAD:
            cells = []
            for loss in LOSSES:
                r = rates(base, arm, off, loss)
                table[(arm, off, loss)] = r
                cells.append(f"{statistics.mean(r) / 1000:7.1f}(n={len(r)})" if r else f"{'-':>13s}")
            print(f"{ARMS[arm]:20s} {off:>7s} " + " ".join(f"{c:>13s}" for c in cells))

    print("\nFold-degradation ratio  rate(0%) / rate(10%)")
    folds = {}
    for arm in ARMS:
        for off in OFFLOAD:
            r0, r10 = table[(arm, off, 0)], table[(arm, off, 10)]
            if r0 and r10:
                fold = statistics.mean(r0) / statistics.mean(r10)
                folds[(arm, off)] = fold
                print(f"  {ARMS[arm]:20s} offload {off:3s}: {fold:6.1f}x")

    print("\nDiscriminator  G = fold(TCP+TLS) / fold(QUIC-control)")
    for off in OFFLOAD:
        if ("tls", off) in folds and ("quic-control", off) in folds:
            g = folds[("tls", off)] / folds[("quic-control", off)]
            print(f"  offload {off:3s}: G = {g:5.2f}")
    if all(("tls", o) in folds and ("quic-control", o) in folds for o in OFFLOAD):
        g_on = folds[("tls", "on")] / folds[("quic-control", "on")]
        g_off = folds[("tls", "off")] / folds[("quic-control", "off")]
        print(f"\n  G(on)={g_on:.2f}  G(off)={g_off:.2f}")
        print("  G(off)~=G(on) and >~2  => QUIC advantage is a real transport property (artifact refuted)")
        print("  G(off)->1            => advantage was a segmentation-offload artifact (confirmed)")

    print("\nOffload sanity: median bytes-per-packet through the broker netem qdisc (want OFF ~= MTU 1500)")
    for arm in ARMS:
        for off in OFFLOAD:
            row = []
            for loss in LOSSES:
                bpp = qdisc_bpp(base, arm, off, loss)
                row.append(f"{bpp:6.0f}" if bpp else f"{'-':>6s}")
            print(f"  {ARMS[arm]:20s} {off:>3s}: " + " ".join(row))

    print("\nBroker CPU (median % of 400); flag > 320 (80% of 4 cores) => CPU-bound, fold suspect")
    for arm in ARMS:
        for off in OFFLOAD:
            row = []
            for loss in LOSSES:
                c = broker_cpu(base, arm, off, loss)
                mark = "*" if c and c > 320 else " "
                row.append(f"{c:5.0f}{mark}" if c else f"{'-':>6s}")
            print(f"  {ARMS[arm]:20s} {off:>3s}: " + " ".join(row))


if __name__ == "__main__":
    default = Path(__file__).resolve().parent.parent / "results-v5"
    main(sys.argv[1] if len(sys.argv) > 1 else default)
