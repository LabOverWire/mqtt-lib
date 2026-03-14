# Experiment Results Manifest

All experiment data for the MQTT-over-QUIC project. The Computer Networks paper (`comnet/`) uses data from `results-phase2` (general experiments) and `results-hol-v3` (HOL blocking).

## Directory Layout

```
experiments/
├── results-phase1/          Original single-VM runs (Feb 2026)
├── results-phase2/          Parallel 9-VM rerun (Mar 2026) ← paper figures
├── results-hol-v3/          HOL methodology redesign (Mar 8-9 2026) ← paper HOL figures
├── results-hol-v4-framepacking/  Frame packing study (Mar 9-10 2026)
└── archive/                 Superseded data with README
```

---

## results-phase1/ (Original Single-VM Runs)

- **Date**: 2026-02-28 to 2026-03-05
- **Infrastructure**: 2 e2-standard-4 VMs (`mqoq-broker`, `mqoq-client`)
- **Branch**: `mqoq-bench-infrastructure`, then `payload-format-experiments`
- **Status**: Superseded by results-phase2 for publication; retained for reproducibility

### Experiments

| Dir | Experiment | Files | Conditions |
|-----|-----------|-------|------------|
| 01_connection_latency | Connection setup time TCP/TLS/QUIC | 75 JSON | 5 transports × 3 RTTs × 5 runs |
| 02_hol_blocking | HOL blocking under packet loss | 80 JSON + traces | 4 transports × 4 loss rates × 5 runs |
| 02b_hol_rtt_sweep | HOL at varying RTTs | 60 JSON + traces | 4 transports × 3 RTTs × 5 runs |
| 02c_hol_topic_scaling | HOL at varying topic counts | 60 JSON + traces | 3 transports × 4 topic counts × 5 runs |
| 02d_hol_rtt_boundary | RTT boundary for stream isolation | 40 JSON + traces | 4 transports × 2 RTTs × 5 runs |
| 02e_hol_qos1 | HOL with QoS 1 | 10 JSON + traces | 2 transports × 1 condition × 5 runs |
| 03_throughput_under_loss | Throughput degradation | 250 JSON | 5 transports × 5 loss rates × 2 QoS × 5 runs |
| 04_stream_strategies | QUIC stream strategy comparison | 125 JSON | 5 strategies × 5 topic counts × 5 runs |
| 12_payload_format_localhost | Payload format performance | 160 JSON | 4 formats × 4 sizes × 2 modes × 5 runs |

---

## results-phase2/ (Parallel 9-VM Rerun)

- **Date**: 2026-03-06 to 2026-03-07
- **Infrastructure**: 9 n2-standard-4 VMs (3 groups of broker+pub+sub)
- **Branch**: `payload-format-experiments`
- **Machine image**: `mqoq-bench-image`
- **Status**: COMPLETE — used for paper figures 02, 03, 08-14

Same experiments as phase1, re-collected with parallel infrastructure for higher statistical power. Each run produces matched triplets: `{label}_run{N}.json` + `_broker_resources.csv` + `_client_resources.csv`.

---

## results-hol-v3/ (HOL Methodology Redesign)

- **Date**: 2026-03-08 to 2026-03-09
- **Infrastructure**: 6 n2-standard-4 VMs (3 groups, co-located pub/sub on client VM)
- **Branch**: `payload-format-experiments`
- **Status**: COMPLETE — used for paper figures 01, 04-07
- **Files**: 240 JSON + 1158 CSV (4 transports × 4 loss rates × 15 runs)

### Methodology changes from v1/v2
- Rate: 500 msg/s (was 5000) — below capacity at all loss rates
- Duration: 60s (was 30), payload: 256B (was 512)
- Always trace (was 0%/1% only)
- Co-located pub/sub on single VM

### Key metrics
- `inter_topic_spread_mean/p95/max_us`: primary HOL discrimination metric
- `detrended_correlation`: first-differences Pearson (removes monotonic trends)
- `spike_isolation_ratio`: fraction of latency spikes isolated to single topic

### Scripts
- Experiment: `parallel/02_hol_blocking_v3.sh`
- Orchestrator: `parallel/run_all_v3.sh`
- Analysis: `analysis/hol_v3_analysis.py`

---

## results-hol-v4-framepacking/ (Frame Packing Study)

- **Date**: 2026-03-09 to 2026-03-10
- **Infrastructure**: 6 n2-standard-4 VMs
- **Branch**: `payload-format-experiments`
- **Quinn fork**: branch `frame-packing-policy-0.11` with `FramePackingPolicy` enum
- **Status**: COMPLETE
- **Files**: 120 JSON + 480 CSV (2 transports × 4 loss rates × 15 runs)

Compares `Greedy` (default) vs `StreamIsolated` frame packing for per-topic and per-publish strategies.

### Scripts
- Experiment: `parallel/02_hol_blocking_v4.sh`
- Orchestrator: `parallel/run_all_v4.sh`
- Analysis: `analysis/hol_v4_frame_packing.py`
- Figure: `analysis/figures/fig15_frame_packing.py`

---

## File Naming Conventions

### JSON result files
`{transport}_{condition}_run{N}.json` — e.g. `quic_pertopic_loss1pct_run3.json`

### CSV trace files
`{transport}_{condition}_run{N}_messages.csv` — per-message latency trace
`{transport}_{condition}_run{N}_quinn_stats.csv` — QUIC connection stats
`{transport}_{condition}_run{N}_broker_resources.csv` — broker CPU/RSS
`{transport}_{condition}_run{N}_client_resources.csv` — client CPU/RSS

---

## Paper Figure Sources

| Figure | Script | Data Source |
|--------|--------|-------------|
| fig01 (spike iso vs loss) | `fig01_spike_iso_vs_loss.py` | results-hol-v3 |
| fig02 (spike iso vs RTT) | `fig02_spike_iso_vs_rtt.py` | results-phase2 |
| fig03 (spike iso vs topics) | `fig03_spike_iso_vs_topics.py` | results-phase2 |
| fig04 (wcorr vs spike iso) | `fig04_wcorr_vs_spike_iso.py` | results-hol-v3 |
| fig05 (latency percentiles) | `fig05_latency_percentiles.py` | results-hol-v3 |
| fig06 (timeseries + CWND) | `fig06_timeseries_cwnd.py` | results-hol-v3 |
| fig07 (inter-arrival) | `fig07_inter_arrival.py` | results-hol-v3 |
| fig08 (connection latency) | `fig08_connection_latency.py` | results-phase2 |
| fig09 (throughput vs loss) | `fig09_throughput_vs_loss.py` | results-phase2 |
| fig10 (resource overhead) | `fig10_resource_overhead.py` | results-phase2 |
| fig11 (QoS comparison) | `fig11_qos_comparison.py` | results-phase2 |
| fig12 (datagram vs stream) | `fig12_datagram_vs_stream.py` | results-phase2 |
| fig13 (resource scaled) | `fig13_resource_overhead_scaled.py` | results-phase2 |
| fig14 (strategy comparison) | `fig14_strategy_comparison.py` | results-phase2 |
| fig15 (frame packing) | `fig15_frame_packing.py` | results-hol-v3 + results-hol-v4-framepacking |

## Statistical Analysis

- `analysis/publication_stats.py` — bootstrap CI, Cliff's delta, LaTeX tables
- `analysis/aggregate.py` — result aggregation across experiment directories
- Generated via: `python3 experiments/analysis/figures/generate_all.py <results_dir> <output_dir>`
