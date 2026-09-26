import argparse
import csv
import random
import sys
from pathlib import Path

LOSSES = [0, 1, 2, 5, 10]
SWEEP_CONFIGS = ["tcp", "tls", "quic-main", "quic-main-ppub", "quic-main-ptopic", "quic-ctl", "quic-ppub"]
LEGACY_CONFIGS = ["tcp", "tls", "quic-main"]
HOL_CONFIGS = ["tcp", "quic-ctl", "quic-ptopic", "quic-ppub"]
FIELDS = ["order", "phase", "mode", "config", "loss", "run", "workload", "router_probe", "broker_probe", "rate"]
CAPPED_CONFIGS = ["quic-ppub", "quic-main-ppub", "quic-ctl"]
CAPPED_RATES = [1250, 2500, 5000, 10000, 20000]
PACED_RATES = [2500, 20000, 0]


def row(phase, mode, config, loss, run, workload="tput", router_probe=0, broker_probe=0, rate=0):
    return {
        "phase": phase, "mode": mode, "config": config, "loss": loss, "run": run, "workload": workload,
        "router_probe": router_probe, "broker_probe": broker_probe, "rate": rate,
    }


def calibration(phase):
    mode_runs = {"calib": ["legacy-calib", "single"], "calib-direct": ["single"]}[phase]
    return [row(phase, mode, config, 0, run) for run in (1, 2, 3) for mode in mode_runs for config in LEGACY_CONFIGS]


def accuracy(rng):
    rows = [row("accuracy", "single", config, loss, 1, router_probe=1)
            for loss in (1, 5, 10) for config in SWEEP_CONFIGS]
    rows += [row("accuracy", "legacy", config, 10, 1, broker_probe=1) for config in LEGACY_CONFIGS]
    rng.shuffle(rows)
    return rows


def main_sweep(rng):
    rows = []
    for run in range(1, 6):
        losses = LOSSES[:]
        rng.shuffle(losses)
        for loss in losses:
            block = [row("main", mode, config, loss, run, router_probe=int(run == 1))
                     for mode in ("single", "router") for config in SWEEP_CONFIGS]
            if run <= 3 and loss > 0:
                block += [row("main", "legacy", config, loss, run) for config in LEGACY_CONFIGS]
            rng.shuffle(block)
            rows += block
    return rows


def router_check(rng):
    rows = [row("routercheck", "router", config, loss, 1, router_probe=1)
            for loss in (0, 1) for config in LEGACY_CONFIGS]
    rng.shuffle(rows)
    return rows


def capped(rng):
    rows = []
    for run in (1, 2, 3):
        block = [row("capped", "router", config, loss, run, rate=rate)
                 for config in CAPPED_CONFIGS for loss in (0, 1) for rate in CAPPED_RATES]
        block += [row("capped", "router", config, loss, run, rate=0) for config in CAPPED_CONFIGS for loss in (0, 1)]
        rng.shuffle(block)
        rows += block
    return rows


def paced(rng):
    rows = []
    for run in (1, 2):
        block = [row("paced", "router", config, loss, run, rate=rate)
                 for config in LEGACY_CONFIGS for loss in (1, 10) for rate in PACED_RATES]
        rng.shuffle(block)
        rows += block
    return rows


def buildcheck(phase, block, rng):
    rows = [row(phase, "router", config, 10, block, rate=0) for config in LEGACY_CONFIGS]
    rng.shuffle(rows)
    return rows


def crosscheck(rng):
    rows = []
    for run in (1, 2, 3):
        block = [row("crosscheck", "tbf", config, loss, run, broker_probe=int(run == 3))
                 for loss in (0, 1, 5, 10) for config in LEGACY_CONFIGS]
        rng.shuffle(block)
        rows += block
    return rows


def hol(rng):
    rows = []
    for run in range(1, 6):
        block = [row("hol", "single", config, loss, run, workload="hol") for loss in (0, 5) for config in HOL_CONFIGS]
        rng.shuffle(block)
        rows += block
    return rows


def build(phase, group, block):
    rng = random.Random(f"03e-{phase}-g{group}-b{block}")
    builders = {
        "calib": lambda: calibration("calib"),
        "calib-direct": lambda: calibration("calib-direct"),
        "accuracy": lambda: accuracy(rng),
        "main": lambda: main_sweep(rng),
        "crosscheck": lambda: crosscheck(rng),
        "routercheck": lambda: router_check(rng),
        "capped": lambda: capped(rng),
        "paced": lambda: paced(rng),
        "hol": lambda: hol(rng),
        "bcfleet": lambda: buildcheck("bcfleet", block, rng),
        "bclater": lambda: buildcheck("bclater", block, rng),
    }
    rows = builders[phase]()
    for index, entry in enumerate(rows, start=1):
        entry["order"] = index
    return rows


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--group", type=int, required=True)
    parser.add_argument("--phase", required=True,
                        choices=["calib", "calib-direct", "accuracy", "routercheck", "main", "crosscheck", "hol", "capped", "paced", "bcfleet", "bclater"])
    parser.add_argument("--block", type=int, default=1)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.out.exists():
        print(f"plan exists, keeping it: {args.out}", file=sys.stderr)
        return
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=FIELDS, lineterminator="\n")
        writer.writeheader()
        writer.writerows(build(args.phase, args.group, args.block))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
