# Experiment Results Manifest

Active results in `experiments/results/`. Archived results in `experiments/archive/`.

## 01_connection_latency

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

## 12_payload_format_localhost

- **Experiment**: Payload format performance on localhost
- **Date**: 2026-03-05
- **Branch**: `payload-format-experiments`
- **Conditions**: 4 formats × 4 payload sizes × 2 modes × 5 runs = 160 JSON files
- **Status**: COMPLETE
