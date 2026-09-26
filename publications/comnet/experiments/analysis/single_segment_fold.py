import csv
import glob
import json
import math
import random
import statistics
import sys
from pathlib import Path

LOSSES = [0, 1, 2, 5, 10]
QUIC_CONFIGS = ["quic-main", "quic-main-ppub", "quic-main-ptopic", "quic-ctl", "quic-ppub"]
BOOTSTRAP = 5000
MAIN_CAMPAIGN_FILES = {
    "tcp": "tcp", "tls": "tls", "quic-main": "quic-control-only",
    "quic-main-ppub": "quic-per-publish", "quic-main-ptopic": "quic-per-topic",
}
TCP_MSS = 1408
PAYLOAD_BYTES = 256
RTT_S = 0.0101
SOFTIRQ = {}
CPU_BOUND = 380.0


def throughput(path):
    try:
        return json.load(open(path))["results"]["throughput_avg"]
    except (json.JSONDecodeError, KeyError, OSError, TypeError):
        return None


def broker_cpu(path):
    try:
        values = [float(row["cpu_percent"]) for row in csv.DictReader(open(path)) if row.get("cpu_percent")]
    except (OSError, ValueError, KeyError):
        return None
    if not values:
        return None
    active = [value for value in values if value >= 0.5 * max(values)]
    return statistics.mean(active)


def broker_softirq(path):
    try:
        rows = [row for row in csv.DictReader(open(path)) if row.get("cpu_percent") and row.get("host_softirq")]
        peak = max(float(row["cpu_percent"]) for row in rows) if rows else 0
        values = [float(row["host_softirq"]) for row in rows if float(row["cpu_percent"]) >= 0.5 * peak]
    except (OSError, ValueError, KeyError):
        return None
    return statistics.mean(values) if values else None


def failing_labels(directory):
    path = directory / "acceptance.csv"
    if not path.exists():
        raise SystemExit(f"{path} missing: run single_segment_accept.py first")
    return {row["label"] for row in csv.DictReader(open(path)) if row["pass"] != "True"}


def per_buffer_drop_fractions(directories):
    fractions = {}
    for directory in directories:
        for row in csv.DictReader(open(directory / "acceptance.csv")):
            if row["phase"] == "main" and row["mode"] == "legacy" and row["pass"] == "True" and row.get("broker_drop_fraction"):
                fractions.setdefault((row["config"], int(row["loss"])), []).append(float(row["broker_drop_fraction"]))
    return {key: statistics.mean(values) for key, values in fractions.items()}


def clustering_prediction(directories, low=1, high=10):
    fractions = per_buffer_drop_fractions(directories)
    keys = [("tls", low), ("tls", high), ("quic-main", low), ("quic-main", high)]
    if any(key not in fractions or fractions[key] <= 0 for key in keys):
        return None
    tls_span = fractions[("tls", high)] / fractions[("tls", low)]
    quic_span = fractions[("quic-main", high)] / fractions[("quic-main", low)]
    return 1 / math.sqrt(tls_span / quic_span)


def load_rerun(directories):
    cells = {}
    cpu = {}
    excluded = []
    for directory in directories:
        manifest = directory / "manifest.csv"
        if not manifest.exists():
            continue
        failing = failing_labels(directory)
        entries = {row["label"]: row for row in csv.DictReader(open(manifest))}.values()
        for entry in entries:
            if entry["phase"] not in ("main", "crosscheck") or entry.get("broker_probe") == "1":
                continue
            value = throughput(directory / f"{entry['label']}.json")
            if entry["label"] in failing or not value or value <= 0:
                excluded.append(entry["label"])
                continue
            key = (entry["mode"], entry["config"], int(entry["loss"]))
            cells.setdefault(key, []).append((directory.name, value))
            load = broker_cpu(directory / f"{entry['label']}_broker_resources.csv")
            if load is not None:
                cpu.setdefault(key, []).append(load)
            softirq = broker_softirq(directory / f"{entry['label']}_broker_resources.csv")
            if softirq is not None:
                SOFTIRQ.setdefault(key, []).append(softirq)
    return cells, cpu, excluded


def load_main_campaign(base):
    cells = {}
    cpu = {}
    for config, prefix in MAIN_CAMPAIGN_FILES.items():
        for loss in LOSSES:
            key = ("main-campaign", config, loss)
            for path in glob.glob(str(base / f"{prefix}_qos0_loss{loss}pct_run*.json")):
                value = throughput(path)
                if value and value > 0:
                    cells.setdefault(key, []).append(("main", value))
                    load = broker_cpu(path.replace(".json", "_broker_resources.csv"))
                    if load is not None:
                        cpu.setdefault(key, []).append(load)
    return cells, cpu


def resample(samples, rng):
    by_group = {}
    for group, value in samples:
        by_group.setdefault(group, []).append(value)
    drawn = [rng.choice(values) for values in by_group.values() for _ in values]
    return statistics.mean(drawn)


def interval(statistic, rng):
    draws = sorted(statistic(rng) for _ in range(BOOTSTRAP))
    return draws[int(0.025 * BOOTSTRAP)], draws[int(0.975 * BOOTSTRAP) - 1]


def mean_of(samples):
    return statistics.mean(value for _, value in samples)


def fold_ratio(cells, mode, reference, quic, low, high):
    needed = [(mode, reference, low), (mode, reference, high), (mode, quic, low), (mode, quic, high)]
    if not all(key in cells for key in needed):
        return None

    def compute(draw):
        ref_low, ref_high, quic_low, quic_high = (draw(cells[key]) for key in needed)
        return (ref_low / ref_high) / (quic_low / quic_high)

    rng = random.Random(f"{mode}-{reference}-{quic}-{low}-{high}")
    return compute(mean_of), interval(lambda r: compute(lambda s: resample(s, r)), rng)


def same_loss_ratio(cells, mode, reference, quic, loss):
    if (mode, reference, loss) not in cells or (mode, quic, loss) not in cells:
        return None
    rng = random.Random(f"R-{mode}-{reference}-{quic}-{loss}")
    point = mean_of(cells[(mode, quic, loss)]) / mean_of(cells[(mode, reference, loss)])
    ci = interval(lambda r: resample(cells[(mode, quic, loss)], r) / resample(cells[(mode, reference, loss)], r), rng)
    return point, ci


def loss_slope(cells, mode, config):
    points = [(math.log(loss / 100), math.log(mean_of(cells[(mode, config, loss)])))
              for loss in (1, 2, 5, 10) if (mode, config, loss) in cells]
    if len(points) < 3:
        return None
    mean_x = statistics.mean(x for x, _ in points)
    mean_y = statistics.mean(y for _, y in points)
    return -sum((x - mean_x) * (y - mean_y) for x, y in points) / sum((x - mean_x) ** 2 for x, _ in points)


def verdict(ci, main_value):
    low, high = ci
    if high < 1:
        return "reversed"
    if low <= 1:
        return "not supported: headline gap was an artifact"
    if low <= main_value <= high:
        return "robust: gap and magnitude hold"
    return "direction robust, magnitude was a netem artifact"


def cross_ratio(cells, numerator, denominator):
    if numerator not in cells or denominator not in cells:
        return None
    rng = random.Random(f"X-{numerator}-{denominator}")
    point = mean_of(cells[numerator]) / mean_of(cells[denominator])
    return point, interval(lambda r: resample(cells[numerator], r) / resample(cells[denominator], r), rng)


def g_shift(cells, arm):
    keys = [(mode, config, loss) for mode in (arm, "main-campaign") for config in ("tls", "quic-main") for loss in (1, 10)]
    if not all(key in cells for key in keys):
        return None

    def compute(draw):
        def g(mode):
            return (draw(cells[(mode, "tls", 1)]) / draw(cells[(mode, "tls", 10)])) / \
                (draw(cells[(mode, "quic-main", 1)]) / draw(cells[(mode, "quic-main", 10)]))
        return g(arm) / g("main-campaign")

    rng = random.Random(f"g-shift-{arm}")
    return compute(mean_of), interval(lambda r: compute(lambda samples: resample(samples, r)), rng)


def cpu_label(cpu, key):
    if key not in cpu:
        return "-"
    load = statistics.mean(cpu[key])
    return f"{load:.0f}{'*' if load >= CPU_BOUND else ''}"


def fmt(result):
    if not result:
        return "n/a"
    point, (low, high) = result
    return f"{point:6.2f} [{low:5.2f}, {high:5.2f}]"


def main(results_dir):
    base = Path(results_dir)
    directories = sorted(base.glob("03e_single_segment_g*"))
    cells, cpu, excluded = load_rerun(directories)
    main_cells, main_cpu = load_main_campaign(base / "03_throughput_under_loss")
    cells.update(main_cells)
    cpu.update(main_cpu)
    print(f"groups: {[d.name for d in directories]}; runs excluded (failed gates or no throughput): {len(excluded)}")
    for label in excluded:
        print(f"  excluded {label}")
    print("main-campaign QUIC cells ran with the broker's default per-topic delivery")

    print("\nDelivered throughput, K msg/s (n) / broker CPU % of 400 (* = CPU-bound, >= 380)")
    modes = ["main-campaign", "legacy", "single", "router", "tbf"]
    for mode in modes:
        for config in ["tcp", "tls"] + QUIC_CONFIGS:
            row = [f"{mean_of(cells[(mode, config, loss)]) / 1000:7.2f}({len(cells[(mode, config, loss)]):2d})"
                   if (mode, config, loss) in cells else f"{'-':>11s}" for loss in LOSSES]
            if any(cell.strip() != "-" for cell in row):
                print(f"  {mode:14s} {config:16s} " + " ".join(row))
                loads = [cpu_label(cpu, (mode, config, loss)) for loss in LOSSES]
                print(f"  {'':14s} {'broker cpu':16s} " + " ".join(f"{load:>11s}" for load in loads))
                if any((mode, config, loss) in SOFTIRQ for loss in LOSSES):
                    irq = [f"{statistics.mean(SOFTIRQ[(mode, config, loss)]):.1f}" if (mode, config, loss) in SOFTIRQ else "-" for loss in LOSSES]
                    print(f"  {'':14s} {'host softirq %':16s} " + " ".join(f"{value:>11s}" for value in irq))

    print("\nLoss-bound fold ratio G' = [X_ref(1)/X_ref(10)] / [X_quic(1)/X_quic(10)], 95% CI blocked by group")
    main_g = fold_ratio(cells, "main-campaign", "tls", "quic-main", 1, 10)
    for mode in modes:
        for reference in ("tls", "tcp"):
            for quic in QUIC_CONFIGS:
                result = fold_ratio(cells, mode, reference, quic, 1, 10)
                if result:
                    loads = ", ".join(cpu_label(cpu, (mode, cfg, loss)) for cfg in (reference, quic) for loss in (1, 10))
                    print(f"  {mode:14s} {reference:4s} vs {quic:16s} G'={fmt(result)}  cpu[{loads}]")

    print("\nHeadline fold G0 = [X(0)/X(10)] ratio (0% level is broker-CPU bound, reported only)")
    for mode in modes:
        result = fold_ratio(cells, mode, "tls", "quic-main", 0, 10)
        if result:
            loads = ", ".join(cpu_label(cpu, (mode, cfg, loss)) for cfg in ("tls", "quic-main") for loss in (0, 10))
            print(f"  {mode:14s} tls vs quic-main G0={fmt(result)}  cpu[{loads}]")

    print("\nSame-loss ratio R(p) = X_quic(p) / X_ref(p), broker CPU of quic/ref cells")
    for mode in modes:
        for reference in ("tls", "tcp"):
            row = [fmt(same_loss_ratio(cells, mode, reference, "quic-main", loss)) for loss in (1, 2, 5, 10)]
            if any(cell != "n/a" for cell in row):
                print(f"  {mode:14s} quic-main/{reference:4s} " + " | ".join(row))
                loads = [f"{cpu_label(cpu, (mode, 'quic-main', loss))}/{cpu_label(cpu, (mode, reference, loss))}" for loss in (1, 2, 5, 10)]
                print(f"  {'':14s} {'cpu':14s} " + " | ".join(f"{load:>20s}" for load in loads))

    print("\nRerun over main campaign, per cell X_single(p) / X_main(p)")
    for config in ("tcp", "tls", "quic-main"):
        row = [fmt(cross_ratio(cells, ("single", config, loss), ("main-campaign", config, loss))) for loss in (1, 2, 5, 10)]
        if any(cell != "n/a" for cell in row):
            print(f"  {config:10s} " + " | ".join(row))

    print("\nLoss exponent b in X ~ p^-b (Mathis predicts 0.5) and G' implied by the slopes, 10^(b_ref - b_quic)")
    for mode in modes:
        slopes = {config: loss_slope(cells, mode, config) for config in ["tcp", "tls"] + QUIC_CONFIGS}
        for config, slope in slopes.items():
            if slope is not None:
                print(f"  {mode:14s} {config:16s} b={slope:5.2f}")
        for reference in ("tls", "tcp"):
            for quic in QUIC_CONFIGS:
                if slopes.get(reference) is not None and slopes.get(quic) is not None:
                    print(f"  {mode:14s} slope-implied G' {reference} vs {quic}: {10 ** (slopes[reference] - slopes[quic]):.2f}")

    print(f"\nMathis constant C = X_bytes * RTT * sqrt(p) / MSS per subscriber connection (Reno ~1.22), "
          f"lower bound using {PAYLOAD_BYTES} B payload, MSS {TCP_MSS}, RTT {RTT_S * 1000:.0f} ms")
    for mode in modes:
        for config in ("tcp", "tls"):
            row = []
            for loss in (1, 2, 5, 10):
                if (mode, config, loss) in cells:
                    per_connection = mean_of(cells[(mode, config, loss)]) / 8
                    row.append(f"{per_connection * PAYLOAD_BYTES * RTT_S * math.sqrt(loss / 100) / TCP_MSS:5.2f}")
                else:
                    row.append(f"{'-':>5s}")
            if any(cell.strip() != "-" for cell in row):
                print(f"  {mode:14s} {config:4s} " + " ".join(row))

    for arm in ("router", "single"):
        result = fold_ratio(cells, arm, "tls", "quic-main", 1, 10)
        if result and main_g:
            print(f"\n{arm} arm vs main campaign, G' tls vs quic-main: {verdict(result[1], main_g[0])}; rerun G'={fmt(result)}")
    rerun = fold_ratio(cells, "single", "tls", "quic-main", 1, 10)
    if rerun and main_g:
        print(f"\nDECISION (pre-registered, G' single-segment tls vs quic-main): {verdict(rerun[1], main_g[0])}")
        print(f"  main-campaign G'={main_g[0]:.2f}; rerun G'={fmt(rerun)}")
        prediction = clustering_prediction(sorted(Path(results_dir).glob("03e_single_segment_g*")))
        if prediction:
            print(f"  clustering-only prediction (X ~ 1/sqrt(p_eff), measured per-buffer drop fractions) for rerun G' / main-campaign G': {prediction:.2f}")
        for arm in ("single", "router", "tbf"):
            print(f"  {arm:6s} G' / main-campaign G' = {fmt(g_shift(cells, arm))}")


if __name__ == "__main__":
    default = Path(__file__).resolve().parent.parent / "results-v5"
    main(sys.argv[1] if len(sys.argv) > 1 else default)
