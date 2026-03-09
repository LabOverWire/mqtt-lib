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
import random
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


def bootstrap_ci(values: list[float], n_resamples: int = 10000, confidence: float = 0.95) -> tuple[float, float]:
    n = len(values)
    if n < 2:
        m = mean(values)
        return m, m
    rng = random.Random(42)
    means = []
    for _ in range(n_resamples):
        sample = [rng.choice(values) for _ in range(n)]
        means.append(mean(sample))
    means.sort()
    alpha = 1 - confidence
    lo_idx = int(math.floor(alpha / 2 * n_resamples))
    hi_idx = int(math.ceil((1 - alpha / 2) * n_resamples)) - 1
    return means[lo_idx], means[hi_idx]


def cliff_delta(group_a: list[float], group_b: list[float]) -> float:
    na, nb = len(group_a), len(group_b)
    if na == 0 or nb == 0:
        return float("nan")
    dominance = 0
    for a in group_a:
        for b in group_b:
            if a > b:
                dominance += 1
            elif a < b:
                dominance -= 1
    return dominance / (na * nb)


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
        "inter_topic_spread_mean_us": results.get("inter_topic_spread_mean_us"),
        "inter_topic_spread_p95_us": results.get("inter_topic_spread_p95_us"),
        "inter_topic_spread_max_us": results.get("inter_topic_spread_max_us"),
        "detrended_correlation": results.get("detrended_correlation"),
        "total_msgs": results["total_messages"],
        "measured_rate": results["measured_rate"],
        "p50_mean": mean(p50s),
        "p95_mean": mean(p95s),
        "p99_mean": mean(p99s),
        "topic_count": len(topics),
        "transport": data["config"].get("transport", "unknown"),
        "stream_strategy": data["config"].get("quic_stream_strategy"),
    }


def extract_conn_metrics(data: dict) -> dict:
    results = data["results"]
    return {
        "p50_connect_us": results["p50_connect_us"],
        "p95_connect_us": results["p95_connect_us"],
        "p99_connect_us": results["p99_connect_us"],
        "connections_per_sec": results["connections_per_sec"],
    }


def extract_throughput_metrics(data: dict) -> dict:
    results = data["results"]
    return {
        "throughput_avg": results["throughput_avg"],
        "published": results["published"],
        "received": results["received"],
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

CONN_PARSERS = {
    "01_connection_latency": (
        re.compile(r"(.+?)_delay(\d+)ms_run(\d+)\.json"),
        lambda m: (m.group(1), f"delay{m.group(2)}ms"),
    ),
}

THROUGHPUT_PARSERS = {
    "03_throughput_under_loss": (
        re.compile(r"(.+?)_qos(\d)_loss(\d+)pct_run(\d+)\.json"),
        lambda m: (m.group(1), f"qos{m.group(2)}_loss{m.group(3)}pct"),
    ),
}

DATAGRAM_PARSERS = {
    "05_datagram_vs_stream": (
        re.compile(r"quic-(datagram|stream)_delay(\d+)ms_loss(\d+)pct_(latency|throughput)_run(\d+)\.json"),
        lambda m: (f"quic-{m.group(1)}", f"delay{m.group(2)}ms_loss{m.group(3)}pct_{m.group(4)}"),
    ),
}

RESOURCE_PARSERS = {
    "06_resource_overhead": (
        re.compile(r"(.+?)_(\d+)conn_run(\d+)\.json"),
        lambda m: (m.group(1), f"{m.group(2)}conn"),
    ),
}

STRATEGY_PARSERS = {
    "04_stream_strategies": (
        re.compile(r"(.+?)_(\d+)topics_(latency|throughput)_run(\d+)\.json"),
        lambda m: (m.group(1), f"{m.group(2)}topics_{m.group(3)}"),
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
    boot_lo, boot_hi = bootstrap_ci(values)
    return {
        "mean": m,
        "std": std(values),
        "ci_lo": lo,
        "ci_hi": hi,
        "bootstrap_ci_lo": boot_lo,
        "bootstrap_ci_hi": boot_hi,
        "n": len(values),
        "values": values,
    }


def aggregate_conn_experiment(base_dir: Path, experiment: str) -> dict[tuple[str, str], list[dict]]:
    exp_dir = base_dir / experiment
    if not exp_dir.exists():
        return {}

    parser_info = CONN_PARSERS.get(experiment)
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
        metrics = extract_conn_metrics(data)
        grouped[(transport, condition)].append(metrics)

    return grouped


def aggregate_throughput_experiment(base_dir: Path, experiment: str) -> dict[tuple[str, str], list[dict]]:
    exp_dir = base_dir / experiment
    if not exp_dir.exists():
        return {}

    parser_info = THROUGHPUT_PARSERS.get(experiment)
    if not parser_info:
        return {}

    pattern, extractor = parser_info
    grouped: dict[tuple[str, str], list[dict]] = defaultdict(list)

    for jf in sorted(exp_dir.glob("*.json")):
        m = pattern.match(jf.name)
        if not m:
            continue
        strategy, condition = extractor(m)
        data = load_json(jf)
        metrics = extract_throughput_metrics(data)
        grouped[(strategy, condition)].append(metrics)

    return grouped


def build_stats_table_generic(
    grouped: dict[tuple[str, str], list[dict]],
    transport_order: list[str],
    conditions: list[str],
    fields: list[str],
) -> dict:
    table = {}
    for transport in transport_order:
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
        vals_a = [m["wcorr"] for m in grouped[key_a] if m.get("wcorr") is not None]
        vals_b = [m["wcorr"] for m in grouped[key_b] if m.get("wcorr") is not None]

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
        cd = cliff_delta(vals_a, vals_b)

        comparisons.append({
            "condition": condition,
            "comparison": label,
            "group_a": f"{key_a[0]}",
            "group_b": f"{key_b[0]}",
            "metric": "wcorr",
            "mean_a": mean(vals_a),
            "mean_b": mean(vals_b),
            "u_statistic": statistic,
            "p_value": p_value,
            "bonferroni_p": bonferroni_p,
            "cohens_d": d if not math.isnan(d) else None,
            "cliff_delta": cd if not math.isnan(cd) else None,
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


def extract_datagram_metrics(data: dict) -> dict:
    results = data["results"]
    mode = data["mode"]
    if mode == "latency":
        return {
            "p50_us": results["p50_us"],
            "p95_us": results["p95_us"],
            "p99_us": results["p99_us"],
            "messages": results["messages"],
        }
    return {
        "throughput_avg": results["throughput_avg"],
        "published": results["published"],
        "received": results["received"],
    }


def aggregate_datagram_experiment(base_dir: Path, experiment: str) -> dict[tuple[str, str], list[dict]]:
    exp_dir = base_dir / experiment
    if not exp_dir.exists():
        return {}

    parser_info = DATAGRAM_PARSERS.get(experiment)
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
        metrics = extract_datagram_metrics(data)
        grouped[(transport, condition)].append(metrics)

    return grouped


def aggregate_resource_experiment(base_dir: Path, experiment: str) -> dict[tuple[str, str], list[dict]]:
    exp_dir = base_dir / experiment
    if not exp_dir.exists():
        return {}

    parser_info = RESOURCE_PARSERS.get(experiment)
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
        metrics = extract_throughput_metrics(data)
        grouped[(transport, condition)].append(metrics)

    return grouped


def aggregate_strategy_experiment(base_dir: Path, experiment: str) -> dict[tuple[str, str], list[dict]]:
    exp_dir = base_dir / experiment
    if not exp_dir.exists():
        return {}

    parser_info = STRATEGY_PARSERS.get(experiment)
    if not parser_info:
        return {}

    pattern, extractor = parser_info
    grouped: dict[tuple[str, str], list[dict]] = defaultdict(list)

    excluded = {"per-subscription"}
    for jf in sorted(exp_dir.glob("*.json")):
        m = pattern.match(jf.name)
        if not m:
            continue
        strategy, condition = extractor(m)
        if strategy in excluded:
            continue
        try:
            data = load_json(jf)
        except (json.JSONDecodeError, ValueError):
            continue
        mode = data["mode"]
        if mode == "latency":
            metrics = {
                "p50_us": data["results"]["p50_us"],
                "p95_us": data["results"]["p95_us"],
                "throughput_avg": None,
            }
        else:
            metrics = {
                "p50_us": None,
                "p95_us": None,
                "throughput_avg": data["results"]["throughput_avg"],
            }
        grouped[(strategy, condition)].append(metrics)

    return grouped


def generate_latex(all_stats: dict, comparisons: list[dict]) -> str:
    lines = []

    lines.append("% Auto-generated by publication_stats.py")
    lines.append("")

    if "exp02" in all_stats:
        lines.append("\\begin{table}[h]")
        lines.append("\\centering")
        lines.append("\\caption{Inter-topic spread (\\textmu{}s) across packet loss rates (25ms RTT, 8 topics)}")
        lines.append("\\label{tab:spread_vs_loss}")
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
                spread = entry.get("inter_topic_spread_mean_us")
                if spread:
                    cells.append(
                        f"${spread['mean']:.0f} \\pm {spread['std']:.0f}$"
                    )
                else:
                    cells.append("---")
            lines.append(" & ".join(cells) + " \\\\")

        lines.append("\\bottomrule")
        lines.append("\\end{tabular}")
        lines.append("\\end{table}")
        lines.append("")

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
        lines.append("\\begin{tabular}{llccccl}")
        lines.append("\\toprule")
        lines.append("Condition & Comparison & $U$ & $p$ (corrected) & Cohen's $d$ & Cliff's $\\delta$ & Sig. \\\\")
        lines.append("\\midrule")

        for comp in comparisons:
            d_str = f"{comp['cohens_d']:.2f}" if comp["cohens_d"] is not None else "---"
            cd_str = f"{comp['cliff_delta']:.2f}" if comp.get("cliff_delta") is not None else "---"
            sig_str = "***" if comp["significant"] else "n.s."
            lines.append(
                f"{comp['condition']} & {comp['comparison']} & "
                f"{comp['u_statistic']:.1f} & {comp['bonferroni_p']:.4f} & "
                f"{d_str} & {cd_str} & {sig_str} \\\\"
            )

        lines.append("\\bottomrule")
        lines.append("\\end{tabular}")
        lines.append("\\end{table}")
        lines.append("")

    if "exp01" in all_stats:
        conn_transport_order = ["tcp", "tls", "quic"]
        conn_labels = {"tcp": "TCP", "tls": "TLS 1.3", "quic": "QUIC"}
        delays = ["delay0ms", "delay25ms", "delay50ms", "delay100ms", "delay200ms"]
        delay_headers = ["0ms", "25ms", "50ms", "100ms", "200ms"]

        lines.append("\\begin{table}[h]")
        lines.append("\\centering")
        lines.append("\\caption{Connection latency p50 (us) across network delays}")
        lines.append("\\label{tab:conn_latency}")
        lines.append("\\begin{tabular}{l" + "c" * len(delays) + "}")
        lines.append("\\toprule")
        lines.append("Transport & " + " & ".join(delay_headers) + " \\\\")
        lines.append("\\midrule")

        for transport in conn_transport_order:
            label = conn_labels.get(transport, transport)
            cells = [label]
            for delay in delays:
                key = f"{transport}_{delay}"
                entry = all_stats["exp01"].get(key, {})
                p50 = entry.get("p50_connect_us")
                if p50:
                    boot_lo = p50.get("bootstrap_ci_lo", p50["ci_lo"])
                    boot_hi = p50.get("bootstrap_ci_hi", p50["ci_hi"])
                    cells.append(f"${p50['mean']:.0f}$ [{boot_lo:.0f}, {boot_hi:.0f}]")
                else:
                    cells.append("---")
            lines.append(" & ".join(cells) + " \\\\")

        lines.append("\\bottomrule")
        lines.append("\\end{tabular}")
        lines.append("\\end{table}")
        lines.append("")

    if "exp03" in all_stats:
        tp_order = ["tcp", "quic-control-only", "quic-per-topic", "quic-per-publish"]
        tp_labels = {
            "tcp": "TCP",
            "quic-control-only": "QUIC control",
            "quic-per-topic": "QUIC per-topic",
            "quic-per-publish": "QUIC per-publish",
        }
        loss_conditions = ["qos0_loss0pct", "qos0_loss1pct", "qos0_loss5pct", "qos0_loss10pct"]
        loss_headers = ["0\\%", "1\\%", "5\\%", "10\\%"]

        lines.append("\\begin{table}[h]")
        lines.append("\\centering")
        lines.append("\\caption{QoS 0 throughput (msgs/sec) across packet loss rates}")
        lines.append("\\label{tab:throughput_qos0}")
        lines.append("\\begin{tabular}{l" + "c" * len(loss_conditions) + "}")
        lines.append("\\toprule")
        lines.append("Strategy & " + " & ".join(loss_headers) + " \\\\")
        lines.append("\\midrule")

        for transport in tp_order:
            label = tp_labels.get(transport, transport)
            cells = [label]
            for cond in loss_conditions:
                key = f"{transport}_{cond}"
                entry = all_stats["exp03"].get(key, {})
                tp_stat = entry.get("throughput_avg")
                if tp_stat:
                    cells.append(f"${tp_stat['mean']:.0f} \\pm {tp_stat['std']:.0f}$")
                else:
                    cells.append("---")
            lines.append(" & ".join(cells) + " \\\\")

        lines.append("\\bottomrule")
        lines.append("\\end{tabular}")
        lines.append("\\end{table}")
        lines.append("")

    if "exp05_latency" in all_stats:
        dgram_labels = {"quic-stream": "Stream", "quic-datagram": "Datagram"}
        delays = ["delay0ms", "delay25ms", "delay50ms"]
        delay_headers = ["0ms", "25ms", "50ms"]
        losses = ["0", "1", "5", "10"]
        loss_headers = ["0\\%", "1\\%", "5\\%", "10\\%"]

        lines.append("\\begin{table}[h]")
        lines.append("\\centering")
        lines.append("\\caption{Datagram vs.~stream p50 latency ($\\mu$s) at 50ms RTT}")
        lines.append("\\label{tab:datagram_latency}")
        lines.append("\\begin{tabular}{l" + "c" * len(losses) + "}")
        lines.append("\\toprule")
        lines.append("Mode & " + " & ".join(loss_headers) + " \\\\")
        lines.append("\\midrule")

        for transport in ["quic-stream", "quic-datagram"]:
            label = dgram_labels[transport]
            cells = [label]
            for loss in losses:
                key = f"{transport}_delay50ms_loss{loss}pct_latency"
                entry = all_stats["exp05_latency"].get(key, {})
                p50 = entry.get("p50_us")
                if p50:
                    cells.append(f"${p50['mean']:.0f} \\pm {p50['std']:.0f}$")
                else:
                    cells.append("---")
            lines.append(" & ".join(cells) + " \\\\")

        lines.append("\\bottomrule")
        lines.append("\\end{tabular}")
        lines.append("\\end{table}")
        lines.append("")

    if "exp06" in all_stats:
        res_labels = {
            "tcp": "TCP",
            "quic-control-only": "QUIC control",
            "quic-per-topic": "QUIC per-topic",
            "quic-per-publish": "QUIC per-publish",
        }
        conns = ["10conn", "50conn", "100conn"]
        conn_headers = ["10", "50", "100"]

        lines.append("\\begin{table}[h]")
        lines.append("\\centering")
        lines.append("\\caption{Throughput (msgs/sec) vs.~connection count}")
        lines.append("\\label{tab:resource_throughput}")
        lines.append("\\begin{tabular}{l" + "c" * len(conns) + "}")
        lines.append("\\toprule")
        lines.append("Strategy & " + " & ".join(conn_headers) + " \\\\")
        lines.append("\\midrule")

        for transport in ["tcp", "quic-control-only", "quic-per-topic", "quic-per-publish"]:
            label = res_labels[transport]
            cells = [label]
            for conn in conns:
                key = f"{transport}_{conn}"
                entry = all_stats["exp06"].get(key, {})
                tp = entry.get("throughput_avg")
                if tp:
                    cells.append(f"${tp['mean']:.0f} \\pm {tp['std']:.0f}$")
                else:
                    cells.append("---")
            lines.append(" & ".join(cells) + " \\\\")

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
    fields = ["spike_iso", "wcorr", "cluster_ratio", "inter_topic_spread_mean_us", "inter_topic_spread_p95_us", "inter_topic_spread_max_us", "detrended_correlation", "measured_rate", "p50_mean", "p95_mean", "p99_mean", "total_msgs"]

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

    exp01 = aggregate_conn_experiment(base_dir, "01_connection_latency")
    if exp01:
        conn_order = ["tcp", "tls", "quic"]
        conn_conditions = sorted({k[1] for k in exp01})
        conn_fields = ["p50_connect_us", "p95_connect_us", "p99_connect_us", "connections_per_sec"]
        all_stats["exp01"] = build_stats_table_generic(exp01, conn_order, conn_conditions, conn_fields)
        print(f"Exp 01 (connection latency): processed ({len(conn_conditions)} delays)")

    exp03 = aggregate_throughput_experiment(base_dir, "03_throughput_under_loss")
    if exp03:
        tp_order = ["tcp", "quic-control-only", "quic-per-topic", "quic-per-publish"]
        tp_conditions = sorted({k[1] for k in exp03})
        tp_fields = ["throughput_avg", "published", "received"]
        all_stats["exp03"] = build_stats_table_generic(exp03, tp_order, tp_conditions, tp_fields)
        print(f"Exp 03 (throughput): processed ({len(tp_conditions)} conditions)")

    exp05 = aggregate_datagram_experiment(base_dir, "05_datagram_vs_stream")
    if exp05:
        dgram_order = ["quic-stream", "quic-datagram"]
        dgram_conditions = sorted({k[1] for k in exp05})
        lat_conditions = [c for c in dgram_conditions if c.endswith("_latency")]
        tp_conditions = [c for c in dgram_conditions if c.endswith("_throughput")]
        dgram_lat_fields = ["p50_us", "p95_us", "p99_us", "messages"]
        dgram_tp_fields = ["throughput_avg", "published", "received"]
        lat_stats = build_stats_table_generic(exp05, dgram_order, lat_conditions, dgram_lat_fields)
        tp_stats = build_stats_table_generic(exp05, dgram_order, tp_conditions, dgram_tp_fields)
        all_stats["exp05_latency"] = lat_stats
        all_stats["exp05_throughput"] = tp_stats
        print(f"Exp 05 (datagram vs stream): processed ({len(dgram_conditions)} conditions)")

    exp06 = aggregate_resource_experiment(base_dir, "06_resource_overhead")
    if exp06:
        res_order = ["tcp", "quic-control-only", "quic-per-topic", "quic-per-publish"]
        res_conditions = sorted({k[1] for k in exp06})
        res_fields = ["throughput_avg", "published", "received"]
        all_stats["exp06"] = build_stats_table_generic(exp06, res_order, res_conditions, res_fields)
        print(f"Exp 06 (resource overhead): processed ({len(res_conditions)} conditions)")

    exp04 = aggregate_strategy_experiment(base_dir, "04_stream_strategies")
    if exp04:
        strat_order = ["control-only", "per-publish", "per-topic"]
        strat_conditions = sorted({k[1] for k in exp04})
        strat_fields = ["p50_us", "p95_us", "throughput_avg"]
        all_stats["exp04"] = build_stats_table_generic(exp04, strat_order, strat_conditions, strat_fields)
        print(f"Exp 04 (stream strategies): processed ({len(strat_conditions)} conditions)")

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

    print("\n=== COMPARISON SUMMARY (wcorr) ===\n")
    for comp in all_comparisons:
        sig = "***" if comp["significant"] else "n.s."
        d_str = f"d={comp['cohens_d']:.2f}" if comp["cohens_d"] is not None else "d=N/A"
        cd_str = f"cd={comp['cliff_delta']:.2f}" if comp.get("cliff_delta") is not None else "cd=N/A"
        print(
            f"  {comp['condition']:12s} {comp['comparison']:20s} "
            f"wcorr: {comp['mean_a']:.3f} vs {comp['mean_b']:.3f}  "
            f"U={comp['u_statistic']:.1f}  p={comp['bonferroni_p']:.4f} {sig}  {d_str}  {cd_str}"
        )

    if "exp02" in all_stats:
        print("\n=== INTER-TOPIC SPREAD SUMMARY ===\n")
        for loss in ["loss0pct", "loss1pct", "loss2pct", "loss5pct"]:
            parts = []
            for transport in TRANSPORT_ORDER:
                key = f"{transport}_{loss}"
                entry = all_stats["exp02"].get(key, {})
                spread = entry.get("inter_topic_spread_mean_us")
                if spread:
                    parts.append(f"{TRANSPORT_LABELS[transport]}: {spread['mean']:.0f}us")
            if parts:
                print(f"  {loss:12s} {', '.join(parts)}")

        print("\n=== SPIKE ISOLATION SUMMARY ===\n")
        for loss in ["loss0pct", "loss1pct", "loss2pct", "loss5pct"]:
            parts = []
            for transport in TRANSPORT_ORDER:
                key = f"{transport}_{loss}"
                entry = all_stats["exp02"].get(key, {})
                si = entry.get("spike_iso")
                if si:
                    parts.append(f"{TRANSPORT_LABELS[transport]}: {si['mean']:.3f}")
            if parts:
                print(f"  {loss:12s} {', '.join(parts)}")


if __name__ == "__main__":
    main()
