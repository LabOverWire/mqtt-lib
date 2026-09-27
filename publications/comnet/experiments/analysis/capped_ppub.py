import csv
import json
import statistics
import sys
from pathlib import Path

CONFIGS = ["quic-ppub", "quic-main-ppub", "quic-ctl"]
LABELS = {
    "quic-ppub": "per-publish delivery (client per-publish)",
    "quic-main-ppub": "per-topic delivery (client per-publish)",
    "quic-ctl": "control-only delivery (client control-only)",
}
RATES = [1250, 2500, 5000, 10000, 20000, 0]
CAP_PROBE_RATES = {10000, 20000}
FANOUT = 8
PACING_TOLERANCE = 0.02
SUB_SATURATION_PREFIX = "P3: sub busy"
METRICS = ["offered", "delivered", "ratio", "cpu", "cpu_us_per_delivered", "cpu_us_per_offered", "pkts_per_delivered"]


def load_json(path):
    try:
        return json.load(open(path))
    except (OSError, json.JSONDecodeError):
        return None


def active_cpu(path):
    try:
        values = [float(row["cpu_percent"]) for row in csv.DictReader(open(path)) if row.get("cpu_percent")]
    except (OSError, ValueError):
        return None
    if not values:
        return None
    return statistics.mean(value for value in values if value >= 0.5 * max(values))


def acceptance(directory):
    path = directory / "acceptance.csv"
    if not path.exists():
        raise SystemExit(f"{path} missing: run single_segment_accept.py {directory} first")
    return {row["label"]: row for row in csv.DictReader(open(path))}


def collect(base):
    cells = {}
    excluded = []
    for directory in sorted(base.glob("03f_capped_g*")):
        accepted = acceptance(directory)
        rows = {row["label"]: row for row in csv.DictReader(open(directory / "manifest.csv"))}.values()
        for row in rows:
            label = row["label"]
            gate = accepted.get(label)
            if gate is None:
                excluded.append((label, "not evaluated by acceptance"))
                continue
            reasons = [reason for reason in gate["failures"].split("; ") if reason]
            if any(not reason.startswith(SUB_SATURATION_PREFIX) for reason in reasons):
                excluded.append((label, gate["failures"]))
                continue
            saturated = bool(reasons)
            pub = load_json(directory / f"{label}_pub.json")
            sub = load_json(directory / f"{label}.json")
            if not pub or not sub:
                excluded.append((label, "missing results"))
                continue
            target = int(row["rate"])
            configured = int(pub.get("config", {}).get("rate", 0) or 0)
            offered = pub["results"].get("offered_rate") or 0.0
            if configured != target:
                excluded.append((label, f"bench rate {configured} != planned {target}"))
                continue
            if target and abs(offered / target - 1) > PACING_TOLERANCE:
                excluded.append((label, f"paced at {offered:.0f}/s, target {target}/s"))
                continue
            delivered = sub["results"]["throughput_avg"]
            received = sub["results"].get("received") or 0
            cpu = None if saturated else active_cpu(directory / f"{label}_broker_resources.csv")
            skbs = float(gate["skbs"]) if gate.get("skbs") else None
            cells.setdefault((row["config"], target, int(row["loss"])), []).append({
                "group": directory.name[-2:],
                "saturated": saturated,
                "offered": offered,
                "delivered": delivered,
                "ratio": delivered / (FANOUT * offered) if offered else None,
                "cpu": cpu,
                "cpu_us_per_delivered": cpu / 100 / delivered * 1e6 if cpu and delivered else None,
                "cpu_us_per_offered": cpu / 100 / offered * 1e6 if cpu and offered else None,
                "pkts_per_delivered": skbs / received if skbs and received else None,
            })
    return cells, excluded


def summary(samples, field):
    values = [s[field] for s in samples if s[field] is not None]
    if not values:
        return "-"
    per_group = {}
    for sample in samples:
        if sample[field] is not None:
            per_group.setdefault(sample["group"], []).append(sample[field])
    groups = " ".join(f"{g}:{statistics.mean(v):.3g}" for g, v in sorted(per_group.items()))
    return f"{statistics.mean(values):.3g} ({groups})"


def paired_differences(cells, loss, field, left, right):
    out = []
    for rate in RATES:
        a, b = cells.get((left, rate, loss)), cells.get((right, rate, loss))
        if not a or not b:
            continue
        diffs = []
        for group in sorted({s["group"] for s in a} & {s["group"] for s in b}):
            va = [s[field] for s in a if s["group"] == group and s[field] is not None]
            vb = [s[field] for s in b if s["group"] == group and s[field] is not None]
            if va and vb:
                diffs.append(statistics.mean(va) / statistics.mean(vb))
        if diffs:
            out.append((rate, statistics.mean(diffs), min(diffs), max(diffs), len(diffs)))
    return out


def main(results_dir):
    cells, excluded = collect(Path(results_dir))
    print(f"excluded runs: {len(excluded)}")
    for label, reason in excluded:
        print(f"  {label}: {reason}")
    for loss in (0, 1):
        print(f"\n=== loss {loss}% (router arm), 8 subscribers. Values: pooled mean (per-group means g1..g3)")
        for config in CONFIGS:
            print(f"  {LABELS[config]}")
            for rate in RATES:
                samples = cells.get((config, rate, loss))
                if not samples:
                    continue
                name = f"rate {rate}" if rate else "uncapped"
                tags = []
                if rate in CAP_PROBE_RATES and config == "quic-ppub":
                    tags.append("cap probe")
                saturated = sum(s["saturated"] for s in samples)
                if saturated:
                    tags.append(f"{saturated} sub-saturated (cpu omitted)")
                print(f"    {name:11s} n={len(samples):2d} {'[' + ', '.join(tags) + ']' if tags else ''}")
                for field in METRICS:
                    print(f"      {field:22s} {summary(samples, field)}")
        for field in ("cpu_us_per_delivered", "pkts_per_delivered"):
            for other in ("quic-main-ppub", "quic-ctl"):
                rows = paired_differences(cells, loss, field, "quic-ppub", other)
                if rows:
                    print(f"  within-group ratio {field}: per-publish / {other}")
                    for rate, mean, low, high, n in rows:
                        name = f"rate {rate}" if rate else "uncapped"
                        print(f"    {name:11s} {mean:5.2f} (range {low:.2f}-{high:.2f}, {n} groups)")


if __name__ == "__main__":
    default = Path(__file__).resolve().parent.parent / "results-v5"
    main(sys.argv[1] if len(sys.argv) > 1 else default)
