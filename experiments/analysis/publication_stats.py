#!/usr/bin/env python3
"""Statistical analysis for publication.

Computes 95% confidence intervals (t-distribution), Mann-Whitney U tests,
Cohen's d effect sizes, and Bonferroni correction for multiple comparisons.

Outputs:
  - publication_stats.json (for figure scripts)
  - publication_tables.tex (LaTeX tables)
  - Console summary

Usage:
    python publication_stats.py <results_base_dir>
"""

import json
import math
import re
import sys
from collections import defaultdict
from pathlib import Path

try:
    from scipy import stats as sp_stats
except ImportError:
    print("ERROR: scipy required. Install with: pip install scipy", file=sys.stderr)
    sys.exit(1)


def load_json(path: Path) -> dict:
    with open(path) as f:
        return json.load(f)


def mean(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0


def std(values: list[float]) -> float:
    if len(values) < 2:
        return 0.0
    m = mean(values)
    return math.sqrt(sum((v - m) ** 2 for v in values) / (len(values) - 1))


def ci_95(values: list[float]) -> tuple[float, float, float]:
    n = len(values)
    if n < 2:
        m = mean(values)
        return m, m, m
    m = mean(values)
    se = std(values) / math.sqrt(n)
    t_crit = float(sp_stats.t.ppf(0.975, df=n - 1))
    return m, m - t_crit * se, m + t_crit * se


def cohens_d(group_a: list[float], group_b: list[float]) -> float:
    na, nb = len(group_a), len(group_b)
    if na < 2 or nb < 2:
        return float("nan")
    ma, mb = mean(group_a), mean(group_b)
    sa, sb = std(group_a), std(group_b)
    pooled_std = math.sqrt(((na - 1) * sa ** 2 + (nb - 1) * sb ** 2) / (na + nb - 2))
    if pooled_std == 0:
        return float("inf") if ma != mb else 0.0
    return (ma - mb) / pooled_std


def extract_metrics(data: dict) -> dict:
    results = data["results"]
    topics = results["topics"]

    p50s = [t["p50_us"] for t in topics]
    p95s = [t["p95_us"] for t in topics]
    p99s = [t["p99_us"] for t in topics]

    return {
        "wcorr": results["windowed_correlation"],
        "spike_iso": results.get("spike_isolation_ratio"),
        "cluster_ratio": results.get("inter_arrival_cluster_ratio"),
        "total_msgs": results["total_messages"],
        "measured_rate": results["measured_rate"],
        "p50_mean": mean(p50s),
        "p95_mean": mean(p95s),
        "p99_mean": mean(p99s),
        "topic_count": len(topics),
        "transport": data["config"].get("transport", "unknown"),
        "stream_strategy": data["config"].get("quic_stream_strategy"),
    }


PARSERS = {
    "02_hol_blocking": (
        re.compile(r"(.+?)_loss(\d+)pct_run(\d+)\.json"),
        lambda m: (m.group(1), f"loss{m.group(2)}pct"),
    ),
    "02b_hol_rtt_sweep": (
        re.compile(r"(.+?)_rtt(\d+)ms_run(\d+)\.json"),
        lambda m: (m.group(1), f"rtt{m.group(2)}ms"),
    ),
    "02c_hol_topic_scaling": (
        re.compile(r"(.+?)_(\d+)topics_run(\d+)\.json"),
        lambda m: (m.group(1), f"{m.group(2)}topics"),
    ),
    "02d_hol_rtt_boundary": (
        re.compile(r"(.+?)_rtt(\d+)ms_run(\d+)\.json"),
        lambda m: (m.group(1), f"rtt{m.group(2)}ms"),
    ),
    "02e_hol_qos1": (
        re.compile(r"(.+?)_qos1_run(\d+)\.json"),
        lambda m: (m.group(1), "qos1"),
    ),
}

TRANSPORT_ORDER = ["tcp", "quic-control", "quic-pertopic", "quic-perpub"]
TRANSPORT_LABELS = {
    "tcp": "TCP",
    "quic-control": "QUIC ctrl",
    "quic-pertopic": "QUIC per-topic",
    "quic-perpub": "QUIC per-pub",
}


def aggregate_experiment(base_dir: Path, experiment: str) -> dict[tuple[str, str], list[dict]]:
    exp_dir = base_dir / experiment
    if not exp_dir.exists():
        return {}

    parser_info = PARSERS.get(experiment)
    if not parser_info:
        return {}

    pattern, extractor = parser_info
    grouped: dict[tuple[str, str], list[dict]] = defaultdict(list)

    for jf in sorted(exp_dir.glob("*.json")):
        m = pattern.match(jf.name)
        if not m:
            continue
        transport, condition = extractor(m)
        data = load_json(jf)
        metrics = extract_metrics(data)
        grouped[(transport, condition)].append(metrics)

    return grouped


def compute_stats(metrics_list: list[dict], field: str) -> dict | None:
    values = [m[field] for m in metrics_list if m.get(field) is not None]
    if not values:
        return None
    m, lo, hi = ci_95(values)
    return {
        "mean": m,
        "std": std(values),
        "ci_lo": lo,
        "ci_hi": hi,
        "n": len(values),
        "values": values,
    }


def build_stats_table(
    grouped: dict[tuple[str, str], list[dict]],
    conditions: list[str],
    fields: list[str],
) -> dict:
    table = {}
    for transport in TRANSPORT_ORDER:
        for condition in conditions:
            key = (transport, condition)
            if key not in grouped:
                continue
            entry = {"transport": transport, "condition": condition}
            for field in fields:
                stat = compute_stats(grouped[key], field)
                if stat:
                    entry[field] = stat
            table[f"{transport}_{condition}"] = entry
    return table


def run_comparisons(grouped: dict, conditions: list[str]) -> list[dict]:
    comparisons = []
    num_comparisons = 0

    pairs = []
    for condition in conditions:
        tcp_key = ("tcp", condition)
        pt_key = ("quic-pertopic", condition)
        if tcp_key in grouped and pt_key in grouped:
            pairs.append((condition, tcp_key, pt_key, "TCP vs per-topic"))

        ctrl_key = ("quic-control", condition)
        if ctrl_key in grouped and pt_key in grouped:
            pairs.append((condition, ctrl_key, pt_key, "ctrl vs per-topic"))

    num_comparisons = len(pairs)

    for condition, key_a, key_b, label in pairs:
        vals_a = [m["spike_iso"] for m in grouped[key_a] if m.get("spike_iso") is not None]
        vals_b = [m["spike_iso"] for m in grouped[key_b] if m.get("spike_iso") is not None]

        if len(vals_a) < 2 or len(vals_b) < 2:
            continue

        all_zero_b = all(v == 0.0 for v in vals_b)
        all_zero_a = all(v == 0.0 for v in vals_a)

        if all_zero_a and all_zero_b:
            p_value = 1.0
            statistic = 0.0
        elif all_zero_b or all_zero_a:
            try:
                statistic, p_value = sp_stats.mannwhitneyu(
                    vals_a, vals_b, alternative="two-sided"
                )
            except ValueError:
                statistic, p_value = 0.0, 1.0
        else:
            statistic, p_value = sp_stats.mannwhitneyu(
                vals_a, vals_b, alternative="two-sided"
            )

        bonferroni_p = min(p_value * num_comparisons, 1.0)
        d = cohens_d(vals_a, vals_b)

        comparisons.append({
            "condition": condition,
            "comparison": label,
            "group_a": f"{key_a[0]}",
            "group_b": f"{key_b[0]}",
            "metric": "spike_iso",
            "mean_a": mean(vals_a),
            "mean_b": mean(vals_b),
            "u_statistic": statistic,
            "p_value": p_value,
            "bonferroni_p": bonferroni_p,
            "cohens_d": d if not math.isnan(d) else None,
            "significant": bonferroni_p < 0.05,
            "n_comparisons": num_comparisons,
        })

    return comparisons


def format_ms(us: float) -> str:
    ms = us / 1000.0
    if ms < 1:
        return f"{us:.0f}$\\mu$s"
    if ms < 100:
        return f"{ms:.1f}ms"
    return f"{ms:.0f}ms"


def generate_latex(all_stats: dict, comparisons: list[dict]) -> str:
    lines = []

    lines.append("% Auto-generated by publication_stats.py")
    lines.append("")

    if "exp02" in all_stats:
        lines.append("\\begin{table}[h]")
        lines.append("\\centering")
        lines.append("\\caption{Spike isolation ratio across packet loss rates (25ms RTT, 8 topics)}")
        lines.append("\\label{tab:spike_iso_vs_loss}")
        lines.append("\\begin{tabular}{lcccc}")
        lines.append("\\toprule")
        lines.append("Strategy & 0\\% loss & 1\\% loss & 2\\% loss & 5\\% loss \\\\")
        lines.append("\\midrule")

        for transport in TRANSPORT_ORDER:
            label = TRANSPORT_LABELS.get(transport, transport)
            cells = [label]
            for loss in ["loss0pct", "loss1pct", "loss2pct", "loss5pct"]:
                key = f"{transport}_{loss}"
                entry = all_stats["exp02"].get(key, {})
                si = entry.get("spike_iso")
                if si:
                    cells.append(
                        f"${si['mean']:.3f} \\pm {si['std']:.3f}$"
                    )
                else:
                    cells.append("---")
            lines.append(" & ".join(cells) + " \\\\")

        lines.append("\\bottomrule")
        lines.append("\\end{tabular}")
        lines.append("\\end{table}")
        lines.append("")

    if comparisons:
        lines.append("\\begin{table}[h]")
        lines.append("\\centering")
        lines.append("\\caption{Statistical comparisons (Mann-Whitney U, Bonferroni-corrected)}")
        lines.append("\\label{tab:comparisons}")
        lines.append("\\begin{tabular}{llcccl}")
        lines.append("\\toprule")
        lines.append("Condition & Comparison & $U$ & $p$ (corrected) & Cohen's $d$ & Sig. \\\\")
        lines.append("\\midrule")

        for comp in comparisons:
            d_str = f"{comp['cohens_d']:.2f}" if comp["cohens_d"] is not None else "---"
            sig_str = "***" if comp["significant"] else "n.s."
            lines.append(
                f"{comp['condition']} & {comp['comparison']} & "
                f"{comp['u_statistic']:.1f} & {comp['bonferroni_p']:.4f} & "
                f"{d_str} & {sig_str} \\\\"
            )

        lines.append("\\bottomrule")
        lines.append("\\end{tabular}")
        lines.append("\\end{table}")
        lines.append("")

    return "\n".join(lines)


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <results_base_dir>", file=sys.stderr)
        sys.exit(1)

    base_dir = Path(sys.argv[1])
    fields = ["spike_iso", "wcorr", "cluster_ratio", "measured_rate", "p50_mean", "p95_mean", "p99_mean", "total_msgs"]

    all_stats = {}
    all_comparisons = []

    exp02 = aggregate_experiment(base_dir, "02_hol_blocking")
    if exp02:
        loss_conditions = ["loss0pct", "loss1pct", "loss2pct", "loss5pct"]
        all_stats["exp02"] = build_stats_table(exp02, loss_conditions, fields)
        all_comparisons.extend(run_comparisons(exp02, loss_conditions))
        print("Exp 02 (loss sweep): processed")

    exp02b = aggregate_experiment(base_dir, "02b_hol_rtt_sweep")
    if exp02b:
        rtt_conditions = ["rtt10ms", "rtt50ms", "rtt100ms"]
        all_stats["exp02b"] = build_stats_table(exp02b, rtt_conditions, fields)
        all_comparisons.extend(run_comparisons(exp02b, rtt_conditions))
        print("Exp 02b (RTT sweep): processed")

    exp02c = aggregate_experiment(base_dir, "02c_hol_topic_scaling")
    if exp02c:
        topic_conditions = ["2topics", "4topics", "16topics", "32topics"]
        all_stats["exp02c"] = build_stats_table(exp02c, topic_conditions, fields)
        all_comparisons.extend(run_comparisons(exp02c, topic_conditions))
        print("Exp 02c (topic scaling): processed")

    exp02d = aggregate_experiment(base_dir, "02d_hol_rtt_boundary")
    if exp02d:
        boundary_conditions = ["rtt15ms", "rtt20ms"]
        all_stats["exp02d"] = build_stats_table(exp02d, boundary_conditions, fields)
        all_comparisons.extend(run_comparisons(exp02d, boundary_conditions))
        print("Exp 02d (RTT boundary): processed")

    exp02e = aggregate_experiment(base_dir, "02e_hol_qos1")
    if exp02e:
        qos_conditions = ["qos1"]
        all_stats["exp02e"] = build_stats_table(exp02e, qos_conditions, fields)
        print("Exp 02e (QoS 1): processed")

    merged_rtt = {}
    if exp02:
        for key, metrics in exp02.items():
            if key[1] == "loss1pct":
                merged_rtt[(key[0], "rtt25ms")] = metrics
    if exp02b:
        merged_rtt.update(exp02b)
    if exp02d:
        merged_rtt.update(exp02d)

    if merged_rtt:
        rtt_all = sorted({k[1] for k in merged_rtt})
        all_stats["merged_rtt"] = build_stats_table(
            merged_rtt, rtt_all, fields
        )
        print(f"Merged RTT data: {len(rtt_all)} RTT points")

    merged_topics = {}
    if exp02:
        for key, metrics in exp02.items():
            if key[1] == "loss1pct":
                topic_count = metrics[0]["topic_count"] if metrics else 8
                merged_topics[(key[0], f"{topic_count}topics")] = metrics
    if exp02c:
        merged_topics.update(exp02c)

    if merged_topics:
        topics_all = sorted({k[1] for k in merged_topics})
        all_stats["merged_topics"] = build_stats_table(
            merged_topics, topics_all, fields
        )
        print(f"Merged topic data: {len(topics_all)} topic counts")

    def sanitize(obj):
        if isinstance(obj, dict):
            return {k: sanitize(v) for k, v in obj.items()}
        if isinstance(obj, list):
            return [sanitize(v) for v in obj]
        if isinstance(obj, float) and (math.isnan(obj) or math.isinf(obj)):
            return None
        import numpy as np
        if isinstance(obj, (np.bool_, np.integer)):
            return obj.item()
        if isinstance(obj, np.floating):
            val = obj.item()
            if math.isnan(val) or math.isinf(val):
                return None
            return val
        return obj

    output_json = sanitize({
        "stats": all_stats,
        "comparisons": all_comparisons,
    })
    json_path = base_dir / "publication_stats.json"
    with open(json_path, "w") as f:
        json.dump(output_json, f, indent=2)
    print(f"\nJSON written to: {json_path}")

    latex = generate_latex(all_stats, all_comparisons)
    tex_path = base_dir / "publication_tables.tex"
    tex_path.write_text(latex)
    print(f"LaTeX written to: {tex_path}")

    print("\n=== COMPARISON SUMMARY ===\n")
    for comp in all_comparisons:
        sig = "***" if comp["significant"] else "n.s."
        d_str = f"d={comp['cohens_d']:.2f}" if comp["cohens_d"] is not None else "d=N/A"
        print(
            f"  {comp['condition']:12s} {comp['comparison']:20s} "
            f"spike_iso: {comp['mean_a']:.3f} vs {comp['mean_b']:.3f}  "
            f"U={comp['u_statistic']:.1f}  p={comp['bonferroni_p']:.4f} {sig}  {d_str}"
        )


if __name__ == "__main__":
    main()
