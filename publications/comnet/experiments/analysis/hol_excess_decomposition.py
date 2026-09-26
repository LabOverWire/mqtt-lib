import csv
import glob
import math
import random
from pathlib import Path
from typing import NamedTuple

import numpy as np
from scipy import stats


class Decomp(NamedTuple):
    n: int
    obs: tuple[float, float]
    null: tuple[float, float]
    excess: tuple[float, float]

SW = 50
TH = 2.0
COW = 10_000_000
N_SHIFTS = 100
NETEM_ONE_WAY_DELAY_US = 25_000

D02 = Path("results-v5/02_hol_blocking")
DSW = Path("results-v5/02c_topic_rate_sweep")


def load_topics(path, nt):
    cols = [[] for _ in range(nt)]
    with open(path) as f:
        for row in csv.DictReader(f):
            ti = int(row["topic_idx"])
            if 0 <= ti < nt:
                cols[ti].append((int(row["receive_ns"]), float(row["latency_us"])))
    out = []
    for d in cols:
        d.sort(key=lambda x: x[0])
        recv = np.array([r for r, _ in d], dtype=np.int64)
        lat = np.array([l for _, l in d], dtype=np.float64)
        out.append((recv, lat))
    return out


def spikes_of(topic_arrays):
    per_topic = []
    for recv, lat in topic_arrays:
        L = len(lat)
        if L <= SW:
            per_topic.append(np.empty(0, dtype=np.int64))
            continue
        W = np.lib.stride_tricks.sliding_window_view(lat, SW)
        med = np.partition(W, SW // 2, axis=1)[:, SW // 2]
        idx = np.arange(SW, L)
        m = med[idx - SW]
        mask = (m > 0) & (lat[idx] > TH * m)
        per_topic.append(recv[idx][mask])
    return per_topic


def co_ratio(times, topics):
    n = len(times)
    if n == 0:
        return 0.0
    co = 0
    j0 = 0
    for i in range(n):
        ti = times[i]
        while times[j0] < ti - COW:
            j0 += 1
        j = j0
        found = False
        while j < n and times[j] <= ti + COW:
            if j != i and topics[j] != topics[i]:
                found = True
                break
            j += 1
        if found:
            co += 1
    return co / n


def flatten(per_topic):
    times = []
    topics = []
    for ti, arr in enumerate(per_topic):
        for v in arr:
            times.append(v)
            topics.append(ti)
    order = np.argsort(times, kind="stable")
    return np.array(times)[order], np.array(topics)[order]


def observed(per_topic):
    t, tp = flatten(per_topic)
    return co_ratio(t, tp)


def null_ratio(per_topic, rng):
    allt = [v for arr in per_topic for v in arr]
    if not allt:
        return 0.0
    lo, hi = min(allt), max(allt)
    span = max(hi - lo, 1)
    vals = []
    for _ in range(N_SHIFTS):
        pt = [((arr - lo + rng.randrange(span)) % span).astype(np.int64) for arr in per_topic]
        t, tp = flatten(pt)
        vals.append(co_ratio(t, tp))
    return sum(vals) / len(vals)


def ci(vals):
    n = len(vals)
    if n < 2:
        return (vals[0] if vals else 0.0), 0.0
    m = sum(vals) / n
    sd = math.sqrt(sum((x - m) ** 2 for x in vals) / (n - 1))
    return m, float(stats.t.ppf(0.975, n - 1)) * sd / math.sqrt(n)


def cell_files(key, topics, rate, loss):
    if topics == 8 and rate == 500:
        return sorted(glob.glob(str(D02 / f"{key}_loss{loss}pct_run*_messages.csv")))
    return sorted(glob.glob(str(DSW / f"{key}_t{topics}_r{rate}_loss{loss}pct_run*_messages.csv")))


def emulation_applied(topic_arrays):
    latencies = np.concatenate([lat for _, lat in topic_arrays if len(lat)])
    return len(latencies) > 0 and float(np.median(latencies)) >= NETEM_ONE_WAY_DELAY_US


def decompose(key, topics, rate, loss):
    files = cell_files(key, topics, rate, loss)
    if not files:
        return None
    rng = random.Random(42)
    obs, nul, exc = [], [], []
    kept = 0
    for fp in files:
        pt = load_topics(fp, topics)
        if not emulation_applied(pt):
            print(f"  excluded (median latency below the emulated delay, netem not applied): {fp}")
            continue
        kept += 1
        sp = spikes_of(pt)
        o = observed(sp)
        nv = null_ratio(sp, rng)
        obs.append(o)
        nul.append(nv)
        exc.append(o - nv)
    if kept == 0:
        return None
    return Decomp(n=kept, obs=ci(obs), null=ci(nul), excess=ci(exc))


def main():
    import json

    transports = ["tcp", "quic-control", "quic-pertopic", "quic-perpub"]
    dump: dict = {}
    for loss in (1, 5):
        dump[loss] = {"topics": {}, "rate": {}}
        for key in transports:
            dump[loss]["topics"][key] = {}
            for t in (2, 4, 8, 16, 32):
                r = decompose(key, t, 500, loss)
                if r:
                    dump[loss]["topics"][key][t] = r._asdict()
            dump[loss]["rate"][key] = {}
            for rate in (125, 250, 500, 1000, 2000):
                r = decompose(key, 8, rate, loss)
                if r:
                    dump[loss]["rate"][key][rate] = r._asdict()
    out = DSW / "decomposition.json"
    with open(out, "w") as f:
        json.dump(dump, f, indent=1)
    print(f"wrote {out}")

    for loss in (1, 5):
        print(f"\n########## LOSS {loss}% ##########")
        print("=== AXIS 1: topics @ rate 500  [obs / null / excess (mean±95%CI over runs)] ===")
        for key in transports:
            print(f"  {key}")
            for t in (2, 4, 8, 16, 32):
                r = decompose(key, t, 500, loss)
                if not r:
                    print(f"    t={t:2d}: (missing)")
                    continue
                print(
                    f"    t={t:2d} n={r.n:2d}: obs={r.obs[0]:.3f}±{r.obs[1]:.3f}"
                    f"  null={r.null[0]:.3f}±{r.null[1]:.3f}"
                    f"  excess={r.excess[0]:+.3f}±{r.excess[1]:.3f}"
                )
        print("=== AXIS 2: rate @ 8 topics ===")
        for key in transports:
            print(f"  {key}")
            for rate in (125, 250, 500, 1000, 2000):
                r = decompose(key, 8, rate, loss)
                if not r:
                    print(f"    r={rate:4d}: (missing)")
                    continue
                print(
                    f"    r={rate:4d} n={r.n:2d}: obs={r.obs[0]:.3f}±{r.obs[1]:.3f}"
                    f"  null={r.null[0]:.3f}±{r.null[1]:.3f}"
                    f"  excess={r.excess[0]:+.3f}±{r.excess[1]:.3f}"
                )


if __name__ == "__main__":
    main()
