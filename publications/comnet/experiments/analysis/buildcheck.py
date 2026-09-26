import csv
import json
import statistics
import sys
from pathlib import Path

CONFIGS = ["tcp", "tls", "quic-main"]
BUILDS = ["bcfleet", "bclater"]
FANOUT = 8


def collect(base):
    cells = {}
    excluded = []
    for directory in sorted(base.glob("03h_buildcheck_g*")):
        accepted = {row["label"]: row for row in csv.DictReader(open(directory / "acceptance.csv"))}
        for row in csv.DictReader(open(directory / "manifest.csv")):
            gate = accepted.get(row["label"])
            if gate is None or gate["pass"] != "True":
                excluded.append((row["label"], gate["failures"] if gate else "not evaluated"))
                continue
            sub = json.load(open(directory / f"{row['label']}.json"))
            pub = json.load(open(directory / f"{row['label']}_pub.json"))
            cells.setdefault((row["phase"], row["config"]), []).append({
                "group": directory.name[-2:],
                "unique": sub["results"]["throughput_avg"] / FANOUT,
                "offered": pub["results"]["offered_rate"],
                "sha": row["binary_sha"],
            })
    return cells, excluded


def group_means(samples, field):
    per_group = {}
    for sample in samples:
        per_group.setdefault(sample["group"], []).append(sample[field])
    return {group: statistics.mean(values) for group, values in per_group.items()}


def within_group_ratio(numerator, denominator):
    left, right = group_means(numerator, "unique"), group_means(denominator, "unique")
    return [left[g] / right[g] for g in sorted(set(left) & set(right))]


def main(results_dir):
    cells, excluded = collect(Path(results_dir))
    print(f"excluded runs: {len(excluded)}")
    for label, reason in excluded:
        print(f"  {label}: {reason}")
    print("\n=== 10% loss, router arm, unpaced. unique msg/s pooled mean (per-group means); offered msg/s")
    for config in CONFIGS:
        for build in BUILDS:
            samples = cells.get((build, config), [])
            if not samples:
                continue
            groups = " ".join(f"{g}:{v:.0f}" for g, v in sorted(group_means(samples, "unique").items()))
            shas = {s["sha"][:8] for s in samples}
            print(f"  {config:10s} {build:8s} n={len(samples)} unique {statistics.mean(s['unique'] for s in samples):7.1f} ({groups}) "
                  f"offered {statistics.mean(s['offered'] for s in samples):9.0f} sha {','.join(sorted(shas))}")
        ratios = within_group_ratio(cells.get(("bclater", config), []), cells.get(("bcfleet", config), []))
        if ratios:
            print(f"  {config:10s} later/fleet {statistics.mean(ratios):.3f} (range {min(ratios):.3f}-{max(ratios):.3f}, {len(ratios)} groups)")
    for build in BUILDS:
        for other in ("tls", "tcp"):
            ratios = within_group_ratio(cells.get((build, "quic-main"), []), cells.get((build, other), []))
            if ratios:
                print(f"  QUIC/{other} {build:8s} {statistics.mean(ratios):.3f} (range {min(ratios):.3f}-{max(ratios):.3f}, {len(ratios)} groups)")


if __name__ == "__main__":
    default = Path(__file__).resolve().parent.parent / "results-v5"
    main(sys.argv[1] if len(sys.argv) > 1 else default)
