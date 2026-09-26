import csv
import glob
import json
import math
import re
import statistics
import sys
from pathlib import Path

WIRE_FRAME_MAX = 1474
ACCOUNTING_TOLERANCE = 0.001
Z_999 = 3.29
NETEM_LIMIT = 100_000
BACKLOG_CEILING = NETEM_LIMIT // 10
ROUTER_CORE_P95_MAX = 60.0
CLIENT_BUSY_P95_MAX = 95.0
CLEAN_RETRANS_MAX = 0.0005
CLEAN_RTO_MAX = 0.00001
MAIN_CAMPAIGN_PREFIX = {"tcp": "tcp", "tls": "tls", "quic-main": "quic-control-only"}
MAIN_CAMPAIGN_GROUP = {"tcp": 2, "tls": 1, "quic-main": 2}
LIMITED_MODES = ("single", "legacy", "tbf")
LEGACY_REFERENCE_03C = {"tls": 0.0895, "quic-main": 0.0768}


def sections(path):
    out = {}
    current = None
    try:
        text = Path(path).read_text()
    except OSError:
        return out
    for line in text.splitlines():
        match = re.match(r"^=== (.+) ===$", line)
        if match:
            current = match.group(1)
            out[current] = []
        elif current:
            out[current].append(line)
    return out


def qdisc_counters(lines, handle):
    for index, line in enumerate(lines):
        if re.match(rf"^qdisc \S+ {re.escape(handle)} ", line):
            block = " ".join(lines[index + 1:index + 4])
            sent = re.search(r"Sent (\d+) bytes (\d+) pkt \(dropped (\d+)", block)
            backlog = re.search(r"backlog \S+ (\d+)p", block)
            if sent:
                return {
                    "bytes": int(sent.group(1)), "pkts": int(sent.group(2)), "dropped": int(sent.group(3)),
                    "backlog": int(backlog.group(1)) if backlog else 0,
                }
    return None


def table_value(lines, table, field):
    rows = [line.split() for line in lines if line.startswith(f"{table}:")]
    for header, values in zip(rows[0::2], rows[1::2]):
        if field in header:
            return int(values[header.index(field)])
    return None


def table_delta(before, after, section, table, field):
    start = table_value(before.get(section, []), table, field)
    end = table_value(after.get(section, []), table, field)
    if start is None or end is None:
        return None
    return end - start


def dev_counters(lines, iface="ens4"):
    for line in lines:
        name, _, rest = line.partition(":")
        if name.strip() == iface:
            fields = rest.split()
            return {"rx_pkts": int(fields[1]), "rx_drop": int(fields[3]), "tx_pkts": int(fields[9]), "tx_drop": int(fields[11])}
    return None


def softnet_column(lines, column):
    total = 0
    for line in lines:
        fields = line.split()
        if len(fields) > column:
            total += int(fields[column], 16)
    return total


def ethtool_drops(lines):
    matched = [int(value) for name, _, value in (line.partition(":") for line in lines)
               if re.search(r"drop|discard|err", name, re.I) and value.strip().isdigit()]
    return sum(matched) if matched else None


def single_number(lines):
    for line in lines:
        if line.strip().isdigit():
            return int(line.strip())
    return None


def probe(path):
    try:
        text = Path(path).read_text()
    except OSError:
        return None
    enq = re.search(r"^@enq: (\d+)", text, re.M)
    if not enq:
        return None
    values = {"enq": int(enq.group(1))}
    for key in ("gso", "maxlen"):
        match = re.search(rf"^@{key}: (\d+)", text, re.M)
        values[key] = int(match.group(1)) if match else 0
    return values


def csv_rows(path):
    try:
        return list(csv.DictReader(open(path)))
    except OSError:
        return []


def p95(values):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(math.ceil(0.95 * len(ordered))) - 1)]


def active_cpu(rows):
    values = []
    for row in rows:
        try:
            values.append(float(row["cpu_percent"]))
        except (KeyError, TypeError, ValueError):
            continue
    if not values:
        return None
    return statistics.mean(value for value in values if value >= 0.5 * max(values))


def throughput(path):
    try:
        results = json.load(open(path))["results"]
        return results.get("throughput_avg", results.get("measured_rate"))
    except (json.JSONDecodeError, KeyError, OSError, TypeError):
        return None


def binomial_bound(loss, skbs):
    return Z_999 * math.sqrt(loss * (1 - loss) / skbs) if skbs and loss > 0 else 0.0


def monitor_integrity(base, failures):
    for name in ("broker", "pub", "sub"):
        rows = csv_rows(f"{base}_{name}_resources.csv")
        stamps = [row["timestamp"] for row in rows]
        if not rows:
            failures.append(f"P5: {name} monitor empty")
        elif abs(len(stamps) - len(set(stamps))) > 1:
            failures.append(f"P5: {name} monitor has {len(stamps)} rows for {len(set(stamps))} timestamps")
    router = csv_rows(f"{base}_router_resources.csv")
    if not router:
        failures.append("P5: router monitor empty")
    else:
        cores = len({row["cpu"] for row in router})
        stamps = len({row["timestamp"] for row in router})
        if abs(len(router) - cores * stamps) > cores:
            failures.append(f"P5: router monitor has {len(router)} rows for {stamps}x{cores}")


def headroom(base, result, failures):
    router = csv_rows(f"{base}_router_resources.csv")
    if router:
        per_core = {}
        for row in router:
            per_core.setdefault(row["cpu"], []).append(float(row["sys_pct"]) + float(row["softirq_pct"]))
        worst = max(p95(values) for values in per_core.values())
        result["router_core_p95"] = worst
        if worst >= ROUTER_CORE_P95_MAX:
            failures.append(f"P3: router core sys+softirq p95 {worst:.1f}%")
    for name in ("pub", "sub"):
        busy = [float(row["cpu_busy"]) for row in csv_rows(f"{base}_{name}_resources.csv") if row.get("cpu_busy")]
        if busy:
            busy_p95 = p95(busy)
            result[f"{name}_busy_p95"] = busy_p95
            if busy_p95 >= CLIENT_BUSY_P95_MAX:
                failures.append(f"P3: {name} busy p95 {busy_p95:.1f}%")
    broker_rows = csv_rows(f"{base}_broker_resources.csv")
    load = active_cpu(broker_rows)
    if load is not None:
        result["broker_cpu_mean"] = round(load, 1)
    for column in ("host_softirq", "host_steal"):
        values = [float(row[column]) for row in broker_rows if row.get(column)]
        if values:
            result[f"broker_{column}_p95"] = p95(values)


def router_gates(base, loss, entry, result, failures):
    guarded = entry.get("router_guard") == "1"
    before = sections(f"{base}_router_before.txt")
    after = sections(f"{base}_router_after.txt")
    rb = qdisc_counters(before.get("tc", []), "10:")
    ra = qdisc_counters(after.get("tc", []), "10:")
    if not (rb and ra):
        failures.append("router netem counters missing")
        return
    sent, dropped = ra["pkts"] - rb["pkts"], ra["dropped"] - rb["dropped"]
    skbs = sent + dropped
    fraction = dropped / skbs if skbs else 0.0
    bound = binomial_bound(loss, skbs)
    result.update(skbs=skbs, drop_fraction=round(fraction, 6), bound=round(bound, 6),
                  bytes_per_pkt=round((ra["bytes"] - rb["bytes"]) / sent, 1) if sent else None)
    if not skbs:
        failures.append("a: no broker traffic crossed the router netem")
    if loss > 0 and abs(fraction - loss) > bound:
        failures.append(f"a: drop fraction {fraction:.5f} outside {loss}±{bound:.5f}")
    if loss == 0 and dropped:
        failures.append(f"a: {dropped} router drops at 0% loss")
    if sent and (ra["bytes"] - rb["bytes"]) / sent > WIRE_FRAME_MAX:
        failures.append("d: bytes per packet above one wire frame")

    probed = probe(f"{base}_router_probe.txt")
    if entry.get("router_probe") == "1" and probed is None:
        failures.append("b/c: router probe output missing")
    if probed:
        expected = skbs + (ra["backlog"] - rb["backlog"])
        result.update(probe_enq=probed["enq"], probe_gso=probed["gso"], probe_maxlen=probed["maxlen"])
        if expected and abs(probed["enq"] - expected) / expected > ACCOUNTING_TOLERANCE:
            failures.append(f"b: probe enq {probed['enq']} vs counters {expected}")
        if probed["gso"] or probed["maxlen"] > WIRE_FRAME_MAX:
            failures.append(f"c: gso skbs {probed['gso']}, max len {probed['maxlen']}")

    ingress_before, ingress_after = single_number(before.get("ingress", [])), single_number(after.get("ingress", []))
    if ingress_before is None or ingress_after is None:
        failures.append("g: router ingress counter missing")
    elif skbs:
        ingress = ingress_after - ingress_before
        result["ingress_over_skbs"] = round(ingress / skbs, 5)
        if not guarded and abs(ingress - skbs) / skbs > ACCOUNTING_TOLERANCE:
            failures.append(f"g: router ingress {ingress} vs netem skbs {skbs}")

    dev_b, dev_a = dev_counters(before.get("dev", [])), dev_counters(after.get("dev", []))
    if not (dev_b and dev_a):
        failures.append("f: router interface counters missing")
    elif dev_a["rx_drop"] - dev_b["rx_drop"] or dev_a["tx_drop"] - dev_b["tx_drop"]:
        failures.append("f: router interface drops")
    if not before.get("softnet") or not after.get("softnet"):
        failures.append("f: router softnet counters missing")
    else:
        dropped_softnet = softnet_column(after["softnet"], 1) - softnet_column(before["softnet"], 1)
        result["router_softnet_squeezed"] = softnet_column(after["softnet"], 2) - softnet_column(before["softnet"], 2)
        if dropped_softnet:
            failures.append(f"f: router softnet drops {dropped_softnet}")
    nic_before, nic_after = ethtool_drops(before.get("ethtool", [])), ethtool_drops(after.get("ethtool", []))
    if nic_before is None or nic_after is None:
        failures.append("f: router NIC drop counters missing")
    elif nic_after - nic_before:
        failures.append(f"f: router NIC drop/error counters +{nic_after - nic_before}")
    for section, table, field in (("snmp", "Ip", "InDiscards"), ("snmp", "Ip", "OutDiscards"),
                                  ("netstat", "IpExt", "InNoRoutes"), ("snmp", "Ip", "OutNoRoutes")):
        change = table_delta(before, after, section, table, field)
        if change is None:
            failures.append(f"f: router {table} {field} missing")
        elif change:
            failures.append(f"f: router {table} {field} +{change}")


def broker_gates(base, loss, entry, result, failures):
    before = sections(f"{base}_broker_before.txt")
    after = sections(f"{base}_broker_after.txt")
    bb = qdisc_counters(before.get("tc", []), "10:")
    ba = qdisc_counters(after.get("tc", []), "10:")
    if not (bb and ba):
        if entry["mode"] != "router":
            failures.append("e: broker netem counters missing")
    else:
        sent, dropped = ba["pkts"] - bb["pkts"], ba["dropped"] - bb["dropped"]
        result["broker_netem_dropped"] = dropped
        if entry["mode"] == "single" and dropped:
            failures.append(f"e: broker delay qdisc dropped {dropped}")
        if entry["mode"] in ("legacy", "tbf") and sent + dropped:
            result["broker_drop_fraction"] = round(dropped / (sent + dropped), 6)
    if entry["mode"] in LIMITED_MODES:
        backlog = [int(row["backlog_pkts"]) for row in csv_rows(f"{base}_broker_backlog.csv") if row.get("backlog_pkts")]
        if not backlog:
            failures.append("e: broker backlog samples missing")
        else:
            result["broker_backlog_max"] = max(backlog)
            if max(backlog) >= BACKLOG_CEILING:
                failures.append(f"e: broker netem backlog reached {max(backlog)} packets")

    retrans = table_delta(before, after, "snmp", "Tcp", "RetransSegs")
    segments = table_delta(before, after, "snmp", "Tcp", "OutSegs")
    if entry["config"] in ("tcp", "tls"):
        if retrans is None or not segments:
            failures.append("P4: broker TCP counters missing")
        else:
            ratio = retrans / segments
            lost = table_delta(before, after, "netstat", "TcpExt", "TCPLostRetransmit")
            timeouts = table_delta(before, after, "netstat", "TcpExt", "TCPTimeouts")
            result["tcp_retrans_ratio"] = round(ratio, 6)
            result["tcp_timeouts"] = timeouts
            result["tcp_lost_retransmit"] = lost
            if lost is None or timeouts is None:
                failures.append("P4: broker TcpExt counters missing")
            if loss == 0 and ratio > CLEAN_RETRANS_MAX:
                failures.append(f"P4: TCP retransmits {ratio:.5f} of segments on a clean path")
            if loss == 0 and lost is not None and lost / segments > CLEAN_RTO_MAX:
                failures.append(f"P4: {lost} lost retransmissions on a clean path")
            if loss == 0 and timeouts is not None and timeouts / segments > CLEAN_RTO_MAX:
                failures.append(f"P4: {timeouts} retransmission timeouts on a clean path")

    if entry["phase"] == "accuracy" and entry["mode"] == "legacy":
        probed = probe(f"{base}_broker_probe.txt")
        fraction = result.get("broker_drop_fraction")
        clustered = fraction is not None and bb and ba and \
            loss - fraction > binomial_bound(loss, (ba["pkts"] - bb["pkts"]) + (ba["dropped"] - bb["dropped"]))
        result["positive_control_gso"] = probed["gso"] if probed else None
        result["positive_control_reference_03c"] = LEGACY_REFERENCE_03C.get(entry["config"])
        if probed and bb and ba:
            counted = (ba["pkts"] - bb["pkts"]) + (ba["dropped"] - bb["dropped"]) + (ba["backlog"] - bb["backlog"])
            result["positive_control_skbs_over_counted"] = round(probed["enq"] / counted, 4) if counted else None
        result["positive_control_detected"] = bool(probed and probed["gso"] > 0 and clustered)


def evaluate(directory, entry):
    label = entry["label"]
    base = directory / label
    loss = float(entry["loss"]) / 100.0
    result = {"label": label, "phase": entry["phase"], "mode": entry["mode"], "config": entry["config"], "loss": entry["loss"]}
    failures = []

    value = throughput(directory / f"{label}.json")
    result["throughput"] = value
    if not value:
        failures.append("throughput missing or zero")
    for key in ("ilb_health", "ilb_health_after"):
        health = entry.get(key, "")
        if not health or any(state != "HEALTHY" for state in health.split()):
            failures.append(f"{key} '{health}'")
    base_delay = 25_000 if entry.get("workload") == "hol" else 10_000
    if int(entry.get("delay_us", 0)) + int(entry.get("router_hop_us", 0)) != base_delay:
        failures.append(f"delay {entry.get('delay_us')}us + hop {entry.get('router_hop_us')}us != {base_delay}us")
    if entry["phase"] == "calib-direct" and entry.get("router_hop_us") != "0":
        failures.append("calib-direct run compensated for a router hop that is not in the path")
    expected_path = "off" if entry["phase"] == "calib-direct" else "on"
    if entry.get("path_state") != expected_path:
        failures.append(f"router path {entry.get('path_state')} (expected {expected_path})")

    if entry["phase"] != "calib-direct":
        router_gates(base, loss if entry["mode"] in ("single", "router") else 0.0, entry, result, failures)
    broker_gates(base, loss, entry, result, failures)
    headroom(base, result, failures)
    monitor_integrity(base, failures)

    result["pass"] = not failures
    result["failures"] = "; ".join(failures)
    return result


def manifest_entries(directory):
    return list({row["label"]: row for row in csv.DictReader(open(directory / "manifest.csv"))}.values())


def main_campaign_cpu(main_dir, prefix):
    loads = [active_cpu(csv_rows(path)) for path in glob.glob(str(main_dir / f"{prefix}_qos0_loss0pct_run*_broker_resources.csv"))]
    loads = [load for load in loads if load is not None]
    return statistics.mean(loads) if loads else None


def calibration_report(directory, results, group):
    main_dir = directory.parent / "03_throughput_under_loss"
    failures = 0
    if not any(result["phase"] in ("calib", "calib-direct") for result in results):
        return failures
    print("  P1 calibration at 0% loss (X = max(5%, 2*CV of the main-campaign cell))")
    for config, prefix in MAIN_CAMPAIGN_PREFIX.items():
        main_values = [v for v in (throughput(p) for p in glob.glob(str(main_dir / f"{prefix}_qos0_loss0pct_run*.json"))) if v]
        arms = {}
        loads = {}
        for result in results:
            if result["config"] == config and result["loss"] == "0" and result["phase"] in ("calib", "calib-direct") and result["throughput"]:
                arms.setdefault((result["phase"], result["mode"]), []).append(result["throughput"])
                if "broker_cpu_mean" in result:
                    loads.setdefault((result["phase"], result["mode"]), []).append(result["broker_cpu_mean"])
        if not main_values:
            print(f"    {config:10s} FAIL: no main-campaign 0% data")
            failures += 1
            continue
        if not arms:
            print(f"    {config:10s} FAIL: no calibration runs")
            failures += 1
            continue
        main_mean = statistics.mean(main_values)
        tolerance = max(0.05, 2 * statistics.stdev(main_values) / main_mean)
        legacy = arms.get(("calib", "legacy-calib"))
        via_router = arms.get(("calib", "single"))
        direct = arms.get(("calib-direct", "single"))
        matched = MAIN_CAMPAIGN_GROUP[config] == group
        if legacy:
            drift = statistics.mean(legacy) / main_mean - 1
            passed = abs(drift) <= tolerance
            if matched and not passed:
                failures += 1
            print(f"    {config:10s} (i) legacy vs main campaign g{MAIN_CAMPAIGN_GROUP[config]}: {drift:+.1%} "
                  f"(tol {tolerance:.1%}) {'PASS' if passed else 'FAIL'}{'' if matched else ' [cross-group, not gated]'}")
            main_load = main_campaign_cpu(main_dir, prefix)
            legacy_load = loads.get(("calib", "legacy-calib"))
            if not (main_load and legacy_load):
                print(f"    {config:10s} (i) broker CPU comparison missing data {'FAIL' if matched else '[cross-group, not gated]'}")
                failures += 1 if matched else 0
            else:
                cpu_drift = statistics.mean(legacy_load) / main_load - 1
                cpu_passed = abs(cpu_drift) <= 0.05
                if matched and not cpu_passed:
                    failures += 1
                print(f"    {config:10s} (i) broker CPU vs main campaign: {cpu_drift:+.1%} (tol 5.0%) "
                      f"{'PASS' if cpu_passed else 'FAIL'}{'' if matched else ' [cross-group, not gated]'}")
        if not (via_router and direct):
            print(f"    {config:10s} (ii)/(iii) FAIL: missing {'router' if not via_router else 'direct'} calibration arm")
            failures += 1
        else:
            gap = statistics.mean(via_router) / statistics.mean(direct) - 1
            passed = abs(gap) <= tolerance
            failures += 0 if passed else 1
            print(f"    {config:10s} (ii) direct vs (iii) router: {gap:+.1%} (tol {tolerance:.1%}) {'PASS' if passed else 'FAIL'}")
        if legacy and via_router:
            print(f"    {config:10s} tail-drop fix effect (iii)/(i): {statistics.mean(via_router) / statistics.mean(legacy) - 1:+.1%}")
    return failures


def main(directory):
    directory = Path(directory)
    if not (directory / "manifest.csv").exists():
        print(f"missing {directory / 'manifest.csv'}")
        return 1
    match = re.search(r"_g(\d+)$", directory.name)
    if not match:
        print(f"cannot read group from {directory.name}")
        return 1
    group = int(match.group(1))
    results = [evaluate(directory, entry) for entry in manifest_entries(directory)]
    fields = sorted({key for result in results for key in result}, key=lambda k: (k != "label", k))
    out = directory / "acceptance.csv"
    with open(out, "w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields, lineterminator="\n")
        writer.writeheader()
        writer.writerows(results)
    failed = [r for r in results if not r["pass"]]
    print(f"{directory.name}: {len(results)} runs, {len(failed)} failing gates -> {out}")
    for result in failed:
        print(f"  FAIL {result['label']}: {result['failures']}")
    for control in (r for r in results if "positive_control_detected" in r):
        print(f"  positive control {control['label']}: detected={control['positive_control_detected']} "
              f"gso={control['positive_control_gso']} drop_fraction={control.get('broker_drop_fraction')} "
              f"(03c reference {control.get('positive_control_reference_03c')}) "
              f"probe skbs / counter segments={control.get('positive_control_skbs_over_counted')}")
    return len(failed) + calibration_report(directory, results, group)


if __name__ == "__main__":
    targets = sys.argv[1:] or sorted(glob.glob(str(Path(__file__).resolve().parent.parent / "results-v5" / "03e_single_segment_g*")))
    sys.exit(1 if sum(main(target) for target in targets) else 0)
