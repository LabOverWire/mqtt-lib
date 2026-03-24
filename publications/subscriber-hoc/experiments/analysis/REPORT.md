# Subscriber-Side Flow Delivery: Experiment Report

## Experiment Setup

**Objective**: Evaluate whether subscriber-side per-topic QUIC stream delivery provides head-of-line (HOL) blocking isolation compared to a single control stream.

**Configuration**:
- Publisher transport: TCP (port 1883)
- Subscriber transport: QUIC (port 14567)
- 8 topics, 500 msg/s target rate, 256-byte payload
- 60-second duration with 5-second warmup per run
- 25ms baseline RTT via netem
- Packet loss rates: 0%, 1%, 2%, 5%
- 15 runs per condition, 2 modes = 120 total runs

**Modes**:
- **Control**: All PUBLISH packets delivered on the QUIC control stream (stream 0)
- **Per-topic flow**: Each topic routed to a dedicated QUIC stream with flow header framing

**Infrastructure**: GCP n2-standard-4 VMs (us-west1-b), mqttv5 v0.26.0 from `subscribe-on-data-flows` branch.

## Data Completeness

120/120 JSON files collected. All 8 conditions (2 modes x 4 loss rates) have exactly 15 runs.

## Key Metrics

### Inter-Topic Spread (ms)

Measures the maximum difference between per-topic mean latencies within 500ms windows. Higher spread under loss indicates topics experience different latencies — evidence of stream-level isolation.

| Loss | Control          | Per-topic flow    | Ratio |
|------|------------------|-------------------|-------|
| 0%   | 0.22 ± 0.01      | 0.23 ± 0.00       | 1.07  |
| 1%   | 1.56 ± 0.07      | 2.79 ± 0.16       | 1.79  |
| 2%   | 3.22 ± 0.11      | 6.10 ± 0.28       | 1.89  |
| 5%   | 6.52 ± 0.20      | 15.78 ± 0.44      | 2.42  |

At 0% loss both modes show identical baseline spread (~0.2ms). Under loss, per-topic flow spread grows 1.8-2.4x faster than control. At 5% loss, per-topic flow reaches 15.8ms spread vs 6.5ms for control — topics on independent streams experience genuinely different retransmission delays.

### Spike Isolation Ratio

Fraction of time windows where all topics spike together (1.0 = perfectly correlated spikes, lower = better isolation). At 0% loss both modes show 0.0 (no spikes).

| Loss | Control          | Per-topic flow    | Ratio |
|------|------------------|-------------------|-------|
| 1%   | 0.979 ± 0.003    | 0.867 ± 0.013     | 0.885 |
| 2%   | 0.979 ± 0.002    | 0.891 ± 0.010     | 0.910 |
| 5%   | 0.978 ± 0.002    | 0.925 ± 0.008     | 0.946 |

Control stream shows near-perfect correlation (0.98) — when one topic spikes, all topics spike together, as expected from shared-stream HOL blocking. Per-topic flow reduces correlated spiking by 5-11%, with the strongest effect at 1% loss (0.867 vs 0.979).

### Latency

| Loss | Metric | Control (ms)  | Per-topic flow (ms) | Ratio |
|------|--------|---------------|---------------------|-------|
| 0%   | p50    | 25.4          | 25.4                | 1.00  |
| 0%   | p95    | 25.9          | 26.0                | 1.00  |
| 1%   | p50    | 25.5          | 25.4                | 1.00  |
| 1%   | p95    | 43.9          | 34.8                | 0.79  |
| 2%   | p50    | 25.6          | 25.5                | 1.00  |
| 2%   | p95    | 55.1          | 49.7                | 0.90  |
| 5%   | p50    | 33.6          | 26.3                | 0.78  |
| 5%   | p95    | 96.9          | 89.9                | 0.93  |

Median latency is identical at 0-2% loss (~25ms, the baseline RTT). Per-topic flow shows clear p95 improvement at 1% loss (35ms vs 44ms, -21%) because retransmission on one stream no longer blocks delivery on other streams. At 5% loss, per-topic flow also wins on p50 (26ms vs 34ms, -22%).

### Throughput

Both modes achieve ~466 msg/s across all loss rates (target: 500 msg/s). The rate-limited design ensures throughput does not confound the latency measurements. No queue buildup observed.

## Detrended Correlation

| Loss | Control          | Per-topic flow    |
|------|------------------|-------------------|
| 0%   | 0.532 ± 0.029    | 0.499 ± 0.035     |
| 1%   | 0.955 ± 0.012    | 0.783 ± 0.042     |
| 2%   | 0.971 ± 0.009    | 0.814 ± 0.030     |
| 5%   | 0.991 ± 0.002    | 0.876 ± 0.024     |

Detrended correlation removes monotonic trends to measure whether topic latencies move together within windows. Control stream shows near-1.0 correlation under loss (all topics affected simultaneously). Per-topic flow reduces this to 0.78-0.88, confirming partial decoupling of topic latencies.

## Summary

Per-topic QUIC stream delivery on the subscriber side provides measurable HOL blocking isolation:

1. **Spread increases 2.4x at 5% loss** — topics experience genuinely different latencies on independent streams
2. **Spike isolation improves 11% at 1% loss** — fewer correlated latency spikes across topics
3. **p95 latency improves 21% at 1% loss** — the most practically relevant condition for real-world deployments
4. **p50 improves 22% at 5% loss** — median latency benefits under heavy loss
5. **Zero overhead at 0% loss** — identical latency and throughput when no loss is present

The isolation effect is strongest at moderate loss (1-2%) where individual stream retransmissions are infrequent enough to create clear per-topic differentiation. At higher loss (5%), shared QUIC congestion control (per-connection CWND) partially masks the isolation benefit, though per-topic flow still outperforms.

## Figures

Generated figures in `figures/output/`:
- `fig01_spread_vs_loss.pdf` — Inter-topic spread vs packet loss rate
- `fig02_spike_isolation.pdf` — Spike isolation ratio vs packet loss rate
- `fig03_latency_percentiles.pdf` — p50 and p95 latency comparison (bar chart)
