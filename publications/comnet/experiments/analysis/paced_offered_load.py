import csv
import json
import statistics
import sys
from pathlib import Path

CONFIGS = ["tcp", "tls", "quic-main"]
RATES = [2500, 20000, 0]
LOSSES = [1, 10]
FANOUT = 8
PACING_TOLERANCE = 0.02
SUB_SATURATION_PREFIX = "P3: sub busy"


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
    for directory in sorted(base.glob("03g_paced_g*")):
        accepted = acceptance(directory)
        for row in {row["label"]: row for row in csv.DictReader(open(directory / "manifest.csv"))}.values():
            label = row["label"]
            gate = accepted.get(label)
            if gate is None:
                excluded.append((label, "not evaluated by acceptance"))
                continue
            reasons = [reason for reason in gate["failures"].split("; ") if reason]
            if any(not reason.startswith(SUB_SATURATION_PREFIX) for reason in reasons):
                excluded.append((label, gate["failures"]))
                continue
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
            cells.setdefault((row["config"], int(row["loss"]), target), []).append({
                "group": directory.name[-2:],
                "saturated": bool(reasons),
                "offered": offered,
                "unique": sub["results"]["throughput_avg"] / FANOUT,
                "cpu": active_cpu(directory / f"{label}_broker_resources.csv"),
            })
    return cells, excluded


def group_means(samples, field):
    per_group = {}
    for sample in samples:
        if sample[field] is not None:
            per_group.setdefault(sample["group"], []).append(sample[field])
    return {group: statistics.mean(values) for group, values in per_group.items()}


def describe(samples, field):
    means = group_means(samples, field)
    if not means:
        return "-"
    pooled = statistics.mean(s[field] for s in samples if s[field] is not None)
    groups = " ".join(f"{g}:{v:.4g}" for g, v in sorted(means.items()))
    return f"{pooled:.4g} ({groups})"


def ratio_by_group(cells, numerator, denominator):
    left, right = group_means(cells.get(numerator, []), "unique"), group_means(cells.get(denominator, []), "unique")
    shared = sorted(set(left) & set(right))
    return [(group, left[group] / right[group]) for group in shared]


def main(results_dir):
    cells, excluded = collect(Path(results_dir))
    print(f"excluded runs: {len(excluded)}")
    for label, reason in excluded:
        print(f"  {label}: {reason}")
    for loss in LOSSES:
        print(f"\n=== loss {loss}% (router arm). Values: pooled mean (per-group means)")
        for config in CONFIGS:
            print(f"  {config}")
            for rate in RATES:
                samples = cells.get((config, loss, rate))
                if not samples:
                    continue
                name = f"paced {rate}" if rate else "unpaced"
                saturated = sum(s["saturated"] for s in samples)
                tag = f" [{saturated} sub-saturated]" if saturated else ""
                print(f"    {name:12s} n={len(samples)}{tag}")
                for field in ("offered", "unique", "cpu"):
                    print(f"      {field:8s} {describe(samples, field)}")
            paced = ratio_by_group(cells, (config, loss, 20000), (config, loss, 0))
            if paced:
                values = [v for _, v in paced]
                print(f"    paced 20000 / unpaced delivered: {statistics.mean(values):.3f} (range {min(values):.3f}-{max(values):.3f}, {len(values)} groups)")
        for rate in RATES:
            for other in ("tls", "tcp"):
                rows = ratio_by_group(cells, ("quic-main", loss, rate), (other, loss, rate))
                if rows:
                    values = [v for _, v in rows]
                    name = f"paced {rate}" if rate else "unpaced"
                    print(f"  QUIC/{other} {name:12s} {statistics.mean(values):.3f} (range {min(values):.3f}-{max(values):.3f}, {len(values)} groups)")


if __name__ == "__main__":
    default = Path(__file__).resolve().parent.parent / "results-v5"
    main(sys.argv[1] if len(sys.argv) > 1 else default)
