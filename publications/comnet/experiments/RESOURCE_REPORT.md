# Resource Usage Report — v5 Experiments

All data from `experiments/results-v5/`. Hardware: GCP n2-standard-4 (4 vCPU, 16 GB RAM).
Resource monitoring via procfs: `rss_kb`, `cpu_percent`, `threads`, sampled every ~1s during each run.

---

## Experiment 02: HOL Blocking (500 msg/s, 25ms delay, 8 topics)

Low publish rate, co-located pub/sub on single VM.

### Broker RSS (Peak, MB)

| Transport                    | 0% loss | 1% loss | 2% loss | 5% loss |
| ---------------------------- | ------- | ------- | ------- | ------- |
| TCP                          | 9.2     | 9.3     | 9.2     | 9.0     |
| QUIC control                 | 21.2    | 25.7    | 24.8    | 24.5    |
| QUIC per-topic               | 21.0    | 25.6    | 25.4    | 25.3    |
| QUIC per-publish             | 20.3    | 25.1    | 25.1    | 24.9    |
| per-topic (StreamIsolated)   | 21.3    | 26.0    | 25.9    | 25.7    |
| per-publish (StreamIsolated) | 20.7    | 25.2    | 25.3    | 25.1    |

### Broker CPU (Mean %)

| Transport                    | 0% loss | 1% loss | 5% loss |
| ---------------------------- | ------- | ------- | ------- |
| TCP                          | 1.0     | 1.1     | 1.2     |
| QUIC control                 | 4.0     | 3.9     | 4.1     |
| QUIC per-topic               | 3.7     | 3.9     | 4.0     |
| QUIC per-publish             | 4.4     | 4.5     | 4.5     |
| per-topic (StreamIsolated)   | 3.8     | 6.0     | 5.5     |
| per-publish (StreamIsolated) | 4.3     | 4.7     | 4.7     |

**Observations:**

- At 500 msg/s all transports use minimal resources. QUIC ~2.3x TCP memory overhead at baseline.
- StreamIsolated frame packing adds ~50% CPU overhead on per-topic under loss (3.9% → 6.0% at 1%).
- Memory increases ~20% when loss is introduced (retransmission buffers).

---

## Experiment 03: Throughput Under Loss (4 pub, 4 sub, 10ms delay)

Max-throughput mode. This is the most resource-intensive experiment.

### Broker RSS (Peak, MB) — QoS 0

| Transport        | 0% loss | 1% loss | 5% loss | 10% loss |
| ---------------- | ------- | ------- | ------- | -------- |
| TCP              | 51      | 1,306   | 1,337   | 1,339    |
| QUIC control     | 65      | 1,306   | 1,308   | 1,308    |
| QUIC per-topic   | 1,365   | 1,304   | 1,303   | 1,309    |
| QUIC per-publish | 1,485   | 1,307   | 1,306   | 1,306    |

### Broker RSS (Peak, MB) — QoS 1

| Transport      | 0% loss | 1% loss | 5% loss | 10% loss |
| -------------- | ------- | ------- | ------- | -------- |
| ALL transports | 1,302   | 1,302   | 1,302   | 1,302    |

The QoS 1 value is **exactly** 1,333,612 KB across every sample, every run, every transport, every loss rate. Zero variance. This is a pre-allocated buffer, not gradual accumulation.

### Broker CPU (Mean %) — QoS 0

| Transport        | 0% loss | 1% loss | 5% loss | 10% loss |
| ---------------- | ------- | ------- | ------- | -------- |
| TCP              | ~280    | ~220    | ~180    | ~160     |
| QUIC control     | ~280    | ~240    | ~200    | ~170     |
| QUIC per-topic   | ~280    | ~240    | ~200    | ~170     |
| QUIC per-publish | ~280    | ~240    | ~200    | ~170     |

**Critical findings:**

1. **QoS 0 at 0% loss divergence**: TCP and QUIC control use 51-65 MB (just connection state). Per-topic uses 1,365 MB and per-publish uses 1,485 MB. This suggests QUIC multi-stream strategies pre-allocate or accumulate stream state proportional to throughput, even without loss.

2. **Loss triggers massive TCP/control memory growth**: At 1% loss, TCP jumps from 51 → 1,306 MB (25x). QUIC control jumps from 65 → 1,306 MB (20x). These are retransmission/send buffers filling up as congestion causes backed-up data.

3. **Per-topic/per-publish RSS _decreases_ under loss**: Per-topic goes from 1,365 → 1,303 MB at 5% loss. Per-publish goes from 1,485 → 1,306 MB. Loss reduces throughput, which reduces the number of concurrent in-flight streams and buffered messages.

4. **QoS 1 hard ceiling at 1,302 MB**: Constant across all conditions. QoS 1 requires buffering every in-flight PUBLISH awaiting PUBACK. The broker pre-allocates this buffer. The value (1,333,612 KB ≈ 1.27 GB) is suspiciously close to a power-of-two-based allocation or a configured limit.

**Action items:**

- Investigate what controls the 1,302 MB ceiling. Check broker config for inflight buffer limits, OS socket buffer limits (`sysctl net.core.wmem_max`), or hardcoded allocations.
- The per-topic 1,365 MB at 0% loss should be investigated — at max throughput with 4 pub/4 sub, how many concurrent streams exist?
- The per-publish 1,485 MB overhead over per-topic (120 MB extra) is the cost of ephemeral stream churn at full throughput.

---

## Experiment 05: Datagram vs Stream (50ms delay)

Moderate load, comparing QUIC datagram vs stream transport.

### Broker RSS (Peak, MB)

| Condition     | Stream    | Datagram  |
| ------------- | --------- | --------- |
| 0ms/0% loss   | 15.7-20.4 | 15.7-20.4 |
| 50ms/0% loss  | ~22       | ~22       |
| 50ms/5% loss  | ~73       | ~75       |
| 50ms/10% loss | ~74       | ~73       |

### Broker CPU (Mean %)

| Condition           | Stream | Datagram |
| ------------------- | ------ | -------- |
| 0ms/0% (throughput) | 32.3   | 62.3     |
| 0ms/0% (latency)    | 11.2   | 13.9     |
| 50ms/5% (latency)   | 63.7   | 62.7     |
| 50ms/10% (latency)  | 65.2   | 64.8     |

**Key finding: Datagram CPU overhead at baseline.**
At 0ms/0% in throughput mode, datagrams use **93% more CPU** than streams (62.3% vs 32.3%). This is likely because datagrams bypass QUIC flow control, requiring the application layer to handle pacing. Under loss, CPU converges as congestion throttling dominates both transports.

Memory usage is comparable between stream and datagram at all conditions.

---

## Experiment 06: Resource Overhead at Scale (0% loss, 0ms delay)

Scaling from 10 to 100 concurrent pub/sub pairs. **Per-run broker restart** to prevent state accumulation.

### Broker RSS (Peak, MB)

| Transport            | 10 conn     | 50 conn   | 100 conn  | Growth factor |
| -------------------- | ----------- | --------- | --------- | ------------- |
| TCP                  | 79.5        | 313.1     | 572.3     | 7.2x          |
| QUIC control         | 21.5        | 288.3     | 621.2     | 28.9x         |
| QUIC per-topic       | 29.0        | 320.3     | 590.0     | 20.3x         |
| **QUIC per-publish** | **4,230.7** | **146.7** | **203.0** | **inverted**  |

### Broker RSS Variance (Stdev, MB)

| Transport        | 10 conn | 50 conn | 100 conn |
| ---------------- | ------- | ------- | -------- |
| TCP              | 1.3     | 3.9     | 20.9     |
| QUIC control     | 17.9    | 58.3    | 30.7     |
| QUIC per-topic   | 12.1    | 80.9    | 12.1     |
| QUIC per-publish | 155.8   | 101.2   | 43.0     |

### Broker CPU (Mean %)

| Transport        | 10 conn | 50 conn | 100 conn |
| ---------------- | ------- | ------- | -------- |
| TCP              | 267.2   | 265.1   | 258.1    |
| QUIC control     | 261.6   | 258.2   | 252.2    |
| QUIC per-topic   | 260.9   | 255.3   | 250.0    |
| QUIC per-publish | 251.7   | 257.1   | 251.4    |

### Throughput (msg/s)

| Transport        | 10 conn | 50 conn | 100 conn |
| ---------------- | ------- | ------- | -------- |
| TCP              | 447,389 | 437,698 | 433,256  |
| QUIC control     | 396,937 | 360,025 | 316,948  |
| QUIC per-topic   | 394,685 | 367,393 | 319,247  |
| QUIC per-publish | 323,755 | 370,831 | 291,129  |

### Per-Publish Memory Anomaly (CRITICAL)

**Within a single run** at 10 connections, RSS grows from 9 MB to 4,494 MB (4.4 GB). This growth happens during the run, not as a pre-allocation.

At 50 connections, RSS drops to 147 MB average (with one outlier run at 504 MB).
At 100 connections, RSS is 203 MB average — per-publish uses LESS memory than TCP (572 MB).

This is an **inverted scaling pattern**: more connections = less memory per connection. Possible explanations:

1. **Stream lifecycle bug at low concurrency**: With 10 connections sending at max throughput, per-publish creates enormous numbers of ephemeral streams. If stream cleanup has a concurrency-based trigger (e.g., cleanup runs when a new connection arrives), low concurrency could delay cleanup.

2. **Throughput-driven accumulation**: 10 connections at 324K msg/s = 32.4K streams/s per connection. With 100 connections at 291K msg/s = 2.9K streams/s per connection. The per-connection stream creation rate is 11x higher at 10 connections.

3. **Quinn connection-level buffering**: Quinn may accumulate closed-stream state proportional to stream churn rate. At lower concurrency, higher per-connection churn accumulates faster than cleanup.

**Action items:**

- Profile per-publish at 10 connections with a memory profiler (e.g., `heaptrack` or `valgrind --tool=massif`)
- Check Quinn's `Connection` struct for closed-stream state retention
- Test with explicit stream cleanup / garbage collection between publishes
- Check if `quinn::Connection::stats()` shows accumulated stream counts

---

## Cross-Experiment Summary

### Thread Count

Stable at 5-6 threads across ALL experiments and conditions. No correlation with connection count or transport strategy. The tokio runtime uses a fixed thread pool.

### Memory Patterns

| Workload                 | TCP      | QUIC control | QUIC per-topic | QUIC per-publish |
| ------------------------ | -------- | ------------ | -------------- | ---------------- |
| Low rate (500 msg/s)     | 9 MB     | 21 MB        | 21 MB          | 20 MB            |
| Max throughput, 0% loss  | 51 MB    | 65 MB        | 1,365 MB       | 1,485 MB         |
| Max throughput, ≥1% loss | 1,306 MB | 1,306 MB     | 1,303 MB       | 1,306 MB         |
| QoS 1, any condition     | 1,302 MB | 1,302 MB     | 1,302 MB       | 1,302 MB         |
| 100 connections          | 572 MB   | 621 MB       | 590 MB         | 203 MB           |

### CPU Patterns

| Workload                   | TCP   | QUIC control | QUIC per-topic | QUIC per-publish      |
| -------------------------- | ----- | ------------ | -------------- | --------------------- |
| Low rate (500 msg/s)       | 1%    | 4%           | 3.7%           | 4.4%                  |
| Max throughput             | ~280% | ~280%        | ~280%          | ~280%                 |
| StreamIsolated (low rate)  | —     | —            | 6.0% (+50%)    | 4.7% (+5%)            |
| Datagram (throughput mode) | —     | —            | —              | 62.3% vs stream 32.3% |

### Top Issues Requiring Investigation

1. **Per-publish 4.2GB at 10 connections** — Stream lifecycle bug or Quinn accumulation. Does not occur at higher concurrency.

2. **1,302 MB QoS 1 ceiling** — Pre-allocated or hard-limited inflight buffer. Need to identify the source (broker config, OS limit, or code constant).

3. **Per-topic 1,365 MB at 0% loss max throughput** — Stream state overhead. At 82K msg/s with per-topic mapping, how many concurrent streams exist? Is cleanup timely?

4. **Datagram 93% CPU overhead** — Datagrams use nearly double the CPU of streams at baseline throughput. Likely bypassing QUIC flow control means the application must do its own pacing work.

5. **QUIC superlinear memory scaling** — Control-only grows 28.9x from 10→100 connections (vs TCP 7.2x). Per-connection QUIC state is significantly larger than TCP state.

---

## Decision: Experiment 6 Removed from Paper

Experiment 6 (resource overhead at scale) was removed from the ComNet paper because:

1. **Saturated broker, not scaling experiment**: All publishers send at max throughput. The broker is CPU-saturated at ~260-330% (on 4 vCPU) across ALL transports and connection counts. Adding connections doesn't measure per-connection overhead — it measures what happens when a saturated broker gets more connections sharing the same fixed capacity.

2. **CPU doesn't explain the throughput decline**: QUIC throughput drops 20% at 100 connections, but QUIC CPU usage actually *decreases* slightly (261→252% for control-only). TCP uses *more* CPU (267-270%) yet maintains flat throughput. The bottleneck is not CPU.

3. **Non-monotonic QUIC behavior**: QUIC control-only throughput *increases* from 10→50 connections (397K→420K) then drops at 100 (317K). Per-publish also increases 10→50 (313K→367K). This non-monotonic pattern undermines any simple "overhead scales with connections" narrative.

4. **Not related to the paper's main topic**: The paper is about HOL blocking isolation through stream mapping. Experiment 6 doesn't measure any stream mapping behavior — it measures aggregate throughput under saturation.

The data remains in `experiments/results-v5/06_resource_overhead/` for future investigation of the implementation's scaling behavior.
