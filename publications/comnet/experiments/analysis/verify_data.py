#!/usr/bin/env python3
"""Verify data integrity across all experiment results.

Walks all JSON files in results/, verifies expected keys exist,
counts runs per condition, flags incomplete conditions, and
verifies cross-experiment references.

Usage:
    python verify_data.py <results_base_dir>
"""

import json
import re
import sys
from collections import defaultdict
from pathlib import Path

REQUIRED_KEYS = ["mode", "config", "results"]
REQUIRED_CONFIG_KEYS = ["duration_secs", "payload_size", "qos"]
HOL_RESULT_KEYS = ["topics", "windowed_correlation", "total_messages", "measured_rate"]
HOL_EXPERIMENTS = {"02_hol_blocking", "02b_hol_rtt_sweep", "02c_hol_topic_scaling",
                   "02d_hol_rtt_boundary", "02e_hol_qos1"}

EXPECTED_RUNS = 5

PARSERS = {
    "02_hol_blocking": (
        re.compile(r"(.+?)_loss(\d+)pct_run(\d+)\.json"),
        lambda m: (m.group(1), f"loss{m.group(2)}pct", int(m.group(3))),
    ),
    "02b_hol_rtt_sweep": (
        re.compile(r"(.+?)_rtt(\d+)ms_run(\d+)\.json"),
        lambda m: (m.group(1), f"rtt{m.group(2)}ms", int(m.group(3))),
    ),
    "02c_hol_topic_scaling": (
        re.compile(r"(.+?)_(\d+)topics_run(\d+)\.json"),
        lambda m: (m.group(1), f"{m.group(2)}topics", int(m.group(3))),
    ),
    "02d_hol_rtt_boundary": (
        re.compile(r"(.+?)_rtt(\d+)ms_run(\d+)\.json"),
        lambda m: (m.group(1), f"rtt{m.group(2)}ms", int(m.group(3))),
    ),
    "02e_hol_qos1": (
        re.compile(r"(.+?)_qos1_run(\d+)\.json"),
        lambda m: (m.group(1), "qos1", int(m.group(2))),
    ),
}


def load_json(path: Path) -> dict | None:
    try:
        with open(path) as f:
            return json.load(f)
    except (json.JSONDecodeError, OSError) as e:
        print(f"  ERROR: {path.name}: {e}")
        return None


def verify_keys(data: dict, is_hol: bool) -> list[str]:
    errors = []
    for key in REQUIRED_KEYS:
        if key not in data:
            errors.append(f"missing top-level key: {key}")

    if "config" in data:
        for key in REQUIRED_CONFIG_KEYS:
            if key not in data["config"]:
                errors.append(f"missing config key: {key}")

    if is_hol and "results" in data:
        for key in HOL_RESULT_KEYS:
            if key not in data["results"]:
                errors.append(f"missing results key: {key}")

    return errors


def verify_experiment(base_dir: Path, experiment: str) -> tuple[int, int, list[str]]:
    exp_dir = base_dir / experiment
    if not exp_dir.exists():
        return 0, 0, [f"directory not found: {experiment}"]

    json_files = sorted(exp_dir.glob("*.json"))
    total = len(json_files)
    issues = []
    condition_runs: dict[tuple[str, str], list[int]] = defaultdict(list)

    parser_info = PARSERS.get(experiment)

    for jf in json_files:
        data = load_json(jf)
        if data is None:
            issues.append(f"{jf.name}: failed to parse JSON")
            continue

        key_errors = verify_keys(data, experiment in HOL_EXPERIMENTS)
        for err in key_errors:
            issues.append(f"{jf.name}: {err}")

        if "results" in data:
            has_spike = "spike_isolation_ratio" in data["results"]
            has_cluster = "inter_arrival_cluster_ratio" in data["results"]
            if has_spike != has_cluster:
                issues.append(f"{jf.name}: has spike_iso but not cluster_ratio (or vice versa)")

        if parser_info:
            pattern, extractor = parser_info
            m = pattern.match(jf.name)
            if m:
                transport, condition, run_num = extractor(m)
                condition_runs[(transport, condition)].append(run_num)

    if parser_info:
        for (transport, condition), runs in sorted(condition_runs.items()):
            if len(runs) < EXPECTED_RUNS:
                issues.append(
                    f"{transport}/{condition}: only {len(runs)} runs "
                    f"(expected {EXPECTED_RUNS}), got runs {sorted(runs)}"
                )
            elif len(runs) > EXPECTED_RUNS:
                issues.append(
                    f"{transport}/{condition}: {len(runs)} runs "
                    f"(expected {EXPECTED_RUNS})"
                )

    return total, len(condition_runs), issues


def verify_cross_references(base_dir: Path) -> list[str]:
    issues = []

    exp02_dir = base_dir / "02_hol_blocking"
    if not exp02_dir.exists():
        issues.append("cannot verify cross-references: 02_hol_blocking missing")
        return issues

    ref_files = list(exp02_dir.glob("*_loss1pct_run*.json"))
    if not ref_files:
        issues.append("no loss1pct files in 02_hol_blocking for cross-reference check")
        return issues

    ref_data = load_json(ref_files[0])
    if ref_data and "results" in ref_data:
        topics = ref_data["results"].get("topics", [])
        if len(topics) != 8:
            issues.append(
                f"02_hol_blocking loss1pct has {len(topics)} topics, expected 8 "
                f"(cross-reference for 02b/02c)"
            )

        if "spike_isolation_ratio" not in ref_data["results"]:
            issues.append(
                "02_hol_blocking loss1pct missing spike_isolation_ratio "
                "(needed for cross-reference)"
            )

    return issues


def print_completeness_matrix(base_dir: Path):
    print("\n=== COMPLETENESS MATRIX ===\n")

    experiments = [
        "01_connection_latency",
        "02_hol_blocking",
        "02b_hol_rtt_sweep",
        "02c_hol_topic_scaling",
        "02d_hol_rtt_boundary",
        "02e_hol_qos1",
        "03_throughput_under_loss",
        "04_stream_strategies",
        "12_payload_format_localhost",
    ]

    for exp in experiments:
        exp_dir = base_dir / exp
        if not exp_dir.exists():
            print(f"  {exp:40s}  NOT FOUND")
            continue
        json_count = len(list(exp_dir.glob("*.json")))
        csv_count = len(list(exp_dir.glob("*.csv")))
        print(f"  {exp:40s}  {json_count:4d} JSON  {csv_count:4d} CSV")


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <results_base_dir>", file=sys.stderr)
        sys.exit(1)

    base_dir = Path(sys.argv[1])
    if not base_dir.exists():
        print(f"ERROR: {base_dir} does not exist", file=sys.stderr)
        sys.exit(1)

    all_ok = True

    experiments = sorted(
        d.name for d in base_dir.iterdir()
        if d.is_dir() and not d.name.startswith(".")
    )

    print(f"Verifying {len(experiments)} experiment directories in {base_dir}\n")

    for exp in experiments:
        total, conditions, issues = verify_experiment(base_dir, exp)
        status = "OK" if not issues else "ISSUES"
        cond_str = f", {conditions} conditions" if conditions else ""
        print(f"  [{status:6s}] {exp}: {total} JSON files{cond_str}")
        for issue in issues:
            all_ok = False
            print(f"           {issue}")

    print("\n=== CROSS-REFERENCE VERIFICATION ===\n")
    xref_issues = verify_cross_references(base_dir)
    if xref_issues:
        all_ok = False
        for issue in xref_issues:
            print(f"  ISSUE: {issue}")
    else:
        print("  OK: 25ms/8-topic reference data present in 02_hol_blocking")

    print_completeness_matrix(base_dir)

    print()
    if all_ok:
        print("ALL CHECKS PASSED")
    else:
        print("ISSUES FOUND (see above)")
        sys.exit(1)


if __name__ == "__main__":
    main()
