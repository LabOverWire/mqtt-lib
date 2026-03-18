# Experiment Results Manifest

Active results in `experiments/results-v5/`. Legacy results in `experiments/results/`.

---

## V5 Results (2026-03-11) — Standardized Parameters

All v5 experiments use: 256B payload, 60s duration, 5s warmup (Exp 1: 30s, 0s warmup), 15 runs per configuration. Results in `experiments/results-v5/`.

### 01_connection_latency (v5)

- **Conditions**: 3 transports (tcp, tls, quic) × 5 delays (0/25/50/100/200ms) × 15 runs = 225 JSON
- **Status**: COMPLETE (quic_delay0ms files are empty — known issue)

### 02_hol_blocking (v5)

- **Conditions**: 4 transports × 4 loss rates × 15 runs = 240 JSON + 2 isolated transports × 4 loss rates × 15 runs = 120 JSON
- **Total**: 360 JSON + 1440 CSV traces
- **Includes**: Greedy and StreamIsolated frame packing variants (quic-pertopic-isolated, quic-perpub-isolated)
- **Status**: COMPLETE

### 02c_hol_topic_scaling (v5)

- **Conditions**: 3 transports × 4 topic counts (2/4/16/32) × 15 runs = 180 JSON + 720 CSV
- **Status**: COMPLETE

### 03_throughput_under_loss (v5)

- **Conditions**: 4 transports × 5 loss rates × 2 QoS × 15 runs = 1200 JSON + 1800 CSV
- **Status**: COMPLETE

### 04_stream_strategies (v5)

- **Conditions**: 3 strategies × 4 topic counts × 2 modes (throughput/latency) × 15 runs = 720 JSON + 1080 CSV
- **Status**: COMPLETE

### 05_datagram_vs_stream (v5)

- **Conditions**: 2 modes × 3 delays × 4 loss rates × 2 QoS × 15 runs = 1440 JSON + 2160 CSV
- **Status**: COMPLETE

### 06_resource_overhead (v5)

- **Conditions**: 4 transports × 3 concurrencies (10/50/100) × 15 runs = 360 JSON + 540 CSV
- **Per-run broker restart** to prevent state accumulation
- **Status**: COMPLETE

### Publication Figures (v5)

- **Scripts**: `experiments/analysis/figures/fig01-fig15_*.py`
- **Generated via**: `python3 experiments/analysis/figures/generate_all.py experiments/results-v5 experiments/analysis/figures/output/`
- **Figures**: fig01-fig10, fig12-fig15 generate from v5 data. fig11 requires 02e (not yet rerun for v5).

---

## Legacy Results (pre-v5)

### 01_connection_latency

- **Experiment**: Connection setup time across TCP, TLS, QUIC at varying RTTs
- **Date**: 2026-02-28
- **Branch**: `mqoq-bench-infrastructure`
- **Conditions**: 5 transports × 3 RTTs × 5 runs = 75 JSON files
- **Status**: COMPLETE

## 02_hol_blocking

- **Experiment**: HOL blocking under packet loss (primary dataset)
- **Date**: 2026-03-04 (re-collected with trace instrumentation)
- **Branch**: `persist-qos2-inflight` (after Phases 1-5 fixes)
- **Conditions**: 4 transports × 4 loss rates (0/1/2/5%) × 5 runs = 80 JSON files
- **Trace data**: `messages.csv` + `quinn_stats.csv` at 0% and 1% loss
- **Key metrics**: `spike_isolation_ratio`, `inter_arrival_cluster_ratio`, `windowed_correlation`
- **Status**: COMPLETE
- **Cross-references**: 25ms/8-topic data at 1% loss provides baseline for experiments 02b and 02c

## 02b_hol_rtt_sweep

- **Experiment**: HOL blocking at varying RTTs (10/50/100ms) with 1% loss
- **Date**: 2026-03-04
- **Conditions**: 4 transports × 3 RTTs × 5 runs = 60 JSON files
- **Trace data**: all runs include traces
- **Status**: COMPLETE
- **Cross-references**: 25ms RTT data comes from exp 02 (loss1pct), not duplicated here

## 02c_hol_topic_scaling

- **Experiment**: HOL blocking at varying topic counts (2/4/16/32) with 25ms/1% loss
- **Date**: 2026-03-05
- **Conditions**: 3 transports (tcp, quic-pertopic, quic-perpub) × 4 topic counts × 5 runs = 60 JSON files
- **Trace data**: all runs include traces
- **Status**: COMPLETE
- **Cross-references**: 8-topic data comes from exp 02 (loss1pct), not duplicated here

## 02d_hol_rtt_boundary

- **Experiment**: RTT boundary for QUIC per-topic stream isolation (15/20ms)
- **Date**: 2026-03-05
- **Branch**: `payload-format-experiments`
- **Conditions**: 4 transports × 2 RTTs (15/20ms) × 5 runs = 40 JSON files
- **Trace data**: all runs include traces
- **Status**: COMPLETE
- **Cross-references**: Combined with exp02 (25ms) and exp02b (10/50/100ms) gives 6-point RTT sweep

## 02e_hol_qos1

- **Experiment**: HOL blocking with QoS 1 acknowledged delivery
- **Date**: 2026-03-05
- **Branch**: `payload-format-experiments`
- **Conditions**: 2 transports (tcp, quic-pertopic) × 1 condition (25ms/1%) × 5 runs = 10 JSON files
- **Trace data**: all runs include traces
- **Status**: COMPLETE

## 03_throughput_under_loss

- **Experiment**: Message throughput degradation under packet loss
- **Date**: 2026-02-28
- **Conditions**: 5 transports × 5 loss rates × 2 QoS × 5 runs = 250 JSON files
- **Status**: COMPLETE

## 04_stream_strategies

- **Experiment**: QUIC stream strategy comparison
- **Date**: 2026-02-28
- **Conditions**: 5 strategies × 5 topic counts × 5 runs = 125 JSON files
- **Status**: COMPLETE (control-only, per-publish, per-topic modes complete)
- **Note**: per-subscription results are identical to per-topic (code alias, deprecated)

## Publication Figures (fig08-fig11)

- **fig08_connection_latency**: Grouped bar chart of p50/p95 connection latency vs network delay (exp01 data)
- **fig09_throughput_vs_loss**: Line plot of throughput vs loss rate for QoS 0 and QoS 1 (exp03 data)
- **fig10_resource_overhead**: Time series of broker RSS and CPU at 1% loss (exp02 broker_resources.csv)
- **fig11_qos_comparison**: Grouped bars comparing QoS 0 vs QoS 1 spike isolation and throughput (exp02 + exp02e data)
- **Scripts**: `experiments/analysis/figures/fig08-11_*.py`
- **Generated via**: `python3 experiments/analysis/figures/generate_all.py experiments/results/ experiments/analysis/figures/output/`

## Statistical Analysis (publication_stats.py)

- **Bootstrap CI**: Distribution-free 95% CI (10k resamples) for all metrics — more robust than t-CI for n=5 bimodal data
- **Cliff's delta**: Non-parametric effect size [-1,1] alongside Cohen's d in comparison tables
- **Exp01 parser**: Connection latency metrics (p50/p95/p99, connections/sec) with LaTeX table
- **Exp03 parser**: Throughput metrics (avg, published, received) with LaTeX table
- **Generated via**: `python3 experiments/analysis/publication_stats.py experiments/results/`

## 12_payload_format_localhost

- **Experiment**: Payload format performance on localhost
- **Date**: 2026-03-05
- **Branch**: `payload-format-experiments`
- **Conditions**: 4 formats × 4 payload sizes × 2 modes × 5 runs = 160 JSON files
- **Status**: COMPLETE
