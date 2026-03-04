#!/usr/bin/env python3
"""Analyze HOL blocking trace data from mqttv5 bench --trace-dir output.

Reads messages.csv and quinn_stats.csv files, produces:
1. Inter-arrival time histogram (frame packing detection)
2. Latency time-series with cwnd overlay (congestion correlation)
3. Per-stream spike independence analysis (HOL blocking detection)
4. Sequence gap analysis (loss/reordering detection)

Usage:
    python hol_trace_analysis.py <results_dir> [--output-dir <dir>]
    python hol_trace_analysis.py /path/to/messages.csv [--stats /path/to/quinn_stats.csv]

The results_dir mode scans for *_messages.csv files and processes each pair.
"""

import argparse
import csv
import sys
from collections import defaultdict
from pathlib import Path


def load_messages(path: Path) -> list[dict]:
    rows = []
    with open(path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            rows.append({
                "topic_idx": int(row["topic_idx"]),
                "seq": int(row["seq"]),
                "publish_ns": int(row["publish_ns"]),
                "receive_ns": int(row["receive_ns"]),
                "latency_us": int(row["latency_us"]),
                "stream_id": int(row["stream_id"]),
            })
    return rows


def load_stats(path: Path) -> list[dict]:
    rows = []
    with open(path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            rows.append({
                "timestamp_ns": int(row["timestamp_ns"]),
                "rtt_us": int(row["rtt_us"]),
                "cwnd": int(row["cwnd"]),
                "lost_packets": int(row["lost_packets"]),
                "congestion_events": int(row["congestion_events"]),
                "sent_packets": int(row["sent_packets"]),
                "stream_data_blocked": int(row["stream_data_blocked"]),
                "data_blocked": int(row["data_blocked"]),
            })
    return rows


def analyze_inter_arrival(messages: list[dict]) -> dict:
    sorted_msgs = sorted(messages, key=lambda m: m["receive_ns"])

    thresholds_us = [10, 50, 100, 500, 1000]
    cross_topic_pairs = 0
    clustered = {t: 0 for t in thresholds_us}

    for i in range(len(sorted_msgs) - 1):
        a = sorted_msgs[i]
        b = sorted_msgs[i + 1]
        if a["topic_idx"] == b["topic_idx"]:
            continue
        cross_topic_pairs += 1
        delta_us = (b["receive_ns"] - a["receive_ns"]) / 1000.0
        for t in thresholds_us:
            if delta_us <= t:
                clustered[t] += 1

    ratios = {}
    for t in thresholds_us:
        ratios[f"within_{t}us"] = clustered[t] / cross_topic_pairs if cross_topic_pairs > 0 else 0.0

    return {
        "cross_topic_adjacent_pairs": cross_topic_pairs,
        "cluster_ratios": ratios,
    }


def analyze_spike_isolation(messages: list[dict]) -> dict:
    by_topic: dict[int, list[dict]] = defaultdict(list)
    for m in messages:
        by_topic[m["topic_idx"]].append(m)

    for topic_msgs in by_topic.values():
        topic_msgs.sort(key=lambda m: m["receive_ns"])

    window = 50
    spike_threshold = 2.0
    co_occurrence_window_us = 10_000

    spikes_by_topic: dict[int, list[dict]] = defaultdict(list)

    for topic_idx, topic_msgs in by_topic.items():
        if len(topic_msgs) < window:
            continue
        latencies = [m["latency_us"] for m in topic_msgs]
        for i in range(window, len(topic_msgs)):
            window_slice = latencies[i - window:i]
            sorted_window = sorted(window_slice)
            median = sorted_window[len(sorted_window) // 2]
            if median > 0 and latencies[i] > spike_threshold * median:
                spikes_by_topic[topic_idx].append(topic_msgs[i])

    all_spikes = []
    for topic_idx, topic_spikes in spikes_by_topic.items():
        for s in topic_spikes:
            all_spikes.append({"topic": topic_idx, "receive_ns": s["receive_ns"]})
    all_spikes.sort(key=lambda s: s["receive_ns"])

    total_spikes = len(all_spikes)
    co_occurring = 0

    for spike in all_spikes:
        has_co_spike = any(
            other["topic"] != spike["topic"]
            and abs(other["receive_ns"] - spike["receive_ns"]) / 1000.0 <= co_occurrence_window_us
            for other in all_spikes
        )
        if has_co_spike:
            co_occurring += 1

    return {
        "total_spikes": total_spikes,
        "spikes_per_topic": {t: len(s) for t, s in spikes_by_topic.items()},
        "co_occurring_spikes": co_occurring,
        "spike_isolation_ratio": co_occurring / total_spikes if total_spikes > 0 else 0.0,
    }


def analyze_sequence_gaps(messages: list[dict]) -> dict:
    by_topic: dict[int, list[int]] = defaultdict(list)
    for m in messages:
        by_topic[m["topic_idx"]].append(m["seq"])

    gaps: dict[int, list[tuple[int, int]]] = {}
    for topic_idx, seqs in by_topic.items():
        sorted_seqs = sorted(seqs)
        topic_gaps = []
        for i in range(1, len(sorted_seqs)):
            diff = sorted_seqs[i] - sorted_seqs[i - 1]
            if diff > 1:
                topic_gaps.append((sorted_seqs[i - 1], sorted_seqs[i]))
        gaps[topic_idx] = topic_gaps

    return {
        "gaps_per_topic": {t: len(g) for t, g in gaps.items()},
        "total_gaps": sum(len(g) for g in gaps.values()),
        "messages_per_topic": {t: len(s) for t, s in by_topic.items()},
    }


def analyze_stream_mapping(messages: list[dict]) -> dict:
    topic_streams: dict[int, set[int]] = defaultdict(set)
    for m in messages:
        topic_streams[m["topic_idx"]].add(m["stream_id"])
    return {
        "streams_per_topic": {t: sorted(s) for t, s in topic_streams.items()},
        "unique_streams": len(set().union(*topic_streams.values())) if topic_streams else 0,
        "unique_topics": len(topic_streams),
    }


def analyze_quinn_stats(stats: list[dict]) -> dict:
    if not stats:
        return {}

    rtts = [s["rtt_us"] for s in stats]
    cwnds = [s["cwnd"] for s in stats]

    last = stats[-1]
    first = stats[0]
    lost_delta = last["lost_packets"] - first["lost_packets"]
    sent_delta = last["sent_packets"] - first["sent_packets"]
    congestion_delta = last["congestion_events"] - first["congestion_events"]

    return {
        "samples": len(stats),
        "duration_s": (last["timestamp_ns"] - first["timestamp_ns"]) / 1e9,
        "rtt_min_us": min(rtts),
        "rtt_max_us": max(rtts),
        "rtt_mean_us": sum(rtts) / len(rtts),
        "cwnd_min": min(cwnds),
        "cwnd_max": max(cwnds),
        "cwnd_mean": sum(cwnds) / len(cwnds),
        "total_lost_packets": lost_delta,
        "total_sent_packets": sent_delta,
        "loss_rate": lost_delta / sent_delta if sent_delta > 0 else 0.0,
        "congestion_events": congestion_delta,
        "stream_data_blocked": last["stream_data_blocked"] - first["stream_data_blocked"],
        "data_blocked": last["data_blocked"] - first["data_blocked"],
    }


def format_report(
    label: str,
    inter_arrival: dict,
    spike: dict,
    seq_gaps: dict,
    stream_map: dict,
    quinn: dict,
) -> str:
    lines = [f"=== {label} ===", ""]

    lines.append("Stream mapping:")
    lines.append(f"  unique streams: {stream_map['unique_streams']}")
    lines.append(f"  unique topics:  {stream_map['unique_topics']}")
    for t, streams in sorted(stream_map["streams_per_topic"].items()):
        lines.append(f"  topic {t}: streams {streams}")
    lines.append("")

    lines.append("Inter-arrival clustering (frame packing indicator):")
    lines.append(f"  cross-topic adjacent pairs: {inter_arrival['cross_topic_adjacent_pairs']}")
    for threshold, ratio in inter_arrival["cluster_ratios"].items():
        lines.append(f"  {threshold}: {ratio:.4f}")
    lines.append("")

    lines.append("Spike isolation (HOL blocking indicator):")
    lines.append(f"  total spikes:       {spike['total_spikes']}")
    lines.append(f"  co-occurring:       {spike['co_occurring_spikes']}")
    lines.append(f"  isolation ratio:    {spike['spike_isolation_ratio']:.4f}")
    lines.append(f"  (1.0 = all spikes co-occur = HOL blocking; ~0 = independent)")
    for t, count in sorted(spike["spikes_per_topic"].items()):
        lines.append(f"  topic {t}: {count} spikes")
    lines.append("")

    lines.append("Sequence gaps (loss/reordering):")
    lines.append(f"  total gaps: {seq_gaps['total_gaps']}")
    for t, count in sorted(seq_gaps["gaps_per_topic"].items()):
        msgs = seq_gaps["messages_per_topic"].get(t, 0)
        lines.append(f"  topic {t}: {count} gaps ({msgs} messages)")
    lines.append("")

    if quinn:
        lines.append("Quinn connection stats:")
        lines.append(f"  samples:            {quinn['samples']}")
        lines.append(f"  duration:           {quinn['duration_s']:.1f}s")
        lines.append(f"  RTT:                {quinn['rtt_min_us']}-{quinn['rtt_max_us']}us (mean {quinn['rtt_mean_us']:.0f}us)")
        lines.append(f"  cwnd:               {quinn['cwnd_min']}-{quinn['cwnd_max']} (mean {quinn['cwnd_mean']:.0f})")
        lines.append(f"  lost packets:       {quinn['total_lost_packets']}/{quinn['total_sent_packets']} ({quinn['loss_rate']:.4f})")
        lines.append(f"  congestion events:  {quinn['congestion_events']}")
        lines.append(f"  stream_data_blocked:{quinn['stream_data_blocked']}")
        lines.append(f"  data_blocked:       {quinn['data_blocked']}")
        lines.append("")

    return "\n".join(lines)


def process_trace_pair(messages_path: Path, stats_path: Path | None, label: str) -> str:
    messages = load_messages(messages_path)
    stats = load_stats(stats_path) if stats_path and stats_path.exists() else []

    inter_arrival = analyze_inter_arrival(messages)
    spike = analyze_spike_isolation(messages)
    seq_gaps = analyze_sequence_gaps(messages)
    stream_map = analyze_stream_mapping(messages)
    quinn = analyze_quinn_stats(stats)

    return format_report(label, inter_arrival, spike, seq_gaps, stream_map, quinn)


def scan_results_dir(results_dir: Path) -> list[tuple[str, Path, Path | None]]:
    pairs = []
    for msg_file in sorted(results_dir.glob("*_messages.csv")):
        label = msg_file.name.replace("_messages.csv", "")
        stats_file = msg_file.parent / f"{label}_quinn_stats.csv"
        pairs.append((label, msg_file, stats_file if stats_file.exists() else None))
    return pairs


def main():
    parser = argparse.ArgumentParser(description="Analyze HOL blocking trace data")
    parser.add_argument("input", help="messages.csv file or results directory")
    parser.add_argument("--stats", help="quinn_stats.csv file (single-file mode)")
    parser.add_argument("--output-dir", help="directory for output reports")
    args = parser.parse_args()

    input_path = Path(args.input)

    if input_path.is_file():
        stats_path = Path(args.stats) if args.stats else None
        label = input_path.stem.replace("_messages", "")
        report = process_trace_pair(input_path, stats_path, label)
        print(report)
        if args.output_dir:
            out = Path(args.output_dir)
            out.mkdir(parents=True, exist_ok=True)
            (out / f"{label}_analysis.txt").write_text(report)
    elif input_path.is_dir():
        pairs = scan_results_dir(input_path)
        if not pairs:
            print(f"no *_messages.csv files found in {input_path}", file=sys.stderr)
            sys.exit(1)
        out_dir = Path(args.output_dir) if args.output_dir else input_path
        out_dir.mkdir(parents=True, exist_ok=True)
        for label, msg_path, stats_path in pairs:
            report = process_trace_pair(msg_path, stats_path, label)
            print(report)
            (out_dir / f"{label}_analysis.txt").write_text(report)
            print(f"wrote {out_dir / f'{label}_analysis.txt'}")
    else:
        print(f"input not found: {input_path}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
