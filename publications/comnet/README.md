# Evaluating Stream Mapping Strategies for MQTT over QUIC

Paper submitted to Computer Networks (Elsevier), March 2026.

## Repository structure

```
comnet/
├── submission/          Flat LaTeX source ready for Editorial Manager
│   ├── main.tex         Single-file manuscript (all sections inlined)
│   ├── references.bib   Bibliography
│   ├── main.bbl         Pre-compiled bibliography
│   ├── main.pdf         Built PDF
│   ├── Figure_1-8.pdf   Publication figures
│   ├── cas-dc.cls       Elsevier CAS double-column class
│   ├── cas-common.sty   Elsevier CAS support package
│   └── cas-model2-names.bst  Bibliography style
│
└── experiments/         Complete experiment infrastructure
    ├── parallel/        Multi-host orchestration scripts (v5)
    ├── run/             Single-host experiment scripts (legacy)
    ├── analysis/        Post-processing and figure generation
    ├── setup/           GCP provisioning and certificate generation
    ├── netem/           Network impairment (tc-netem)
    ├── monitor/         Broker/client resource sampling
    ├── terraform/       GCP infrastructure-as-code
    ├── k8s/             Kubernetes manifests (Phase 2, unused)
    ├── archive/         Superseded preliminary data
    └── results-v5/      Active experiment results (1.3 GB, gitignored)
```

## Paper overview

The paper defines three configurable stream mapping strategies for running MQTT over QUIC and evaluates them across five experiments:

| # | Experiment | What it measures |
|---|-----------|-----------------|
| 1 | Connection latency | Cold-start handshake time: TCP vs TLS 1.3 vs QUIC |
| 2 | Head-of-line blocking | Inter-topic latency isolation under packet loss |
| 3 | Throughput under loss | Sustained message rate across loss rates (0-10%) |
| 4 | Stream strategy comparison | Latency and throughput vs topic count |
| 5 | Datagram vs stream | QUIC datagram transport vs stream-based delivery |

An ablation study (Experiment 2) additionally evaluates a custom Quinn frame packing policy (StreamIsolated vs Greedy) to isolate implementation-level coupling from protocol-level behavior.

## Figure-to-script mapping

| Paper figure | Script | Data source |
|-------------|--------|-------------|
| Figure 1 — Inter-topic spread | `fig01_spike_iso_vs_loss.py` | `results-v5/02_hol_blocking/` |
| Figure 2 — Spike isolation ratio | `fig04_wcorr_vs_spike_iso.py` | `results-v5/02_hol_blocking/` |
| Figure 3 — Latency percentiles | `fig05_latency_percentiles.py` | `results-v5/02_hol_blocking/` |
| Figure 4 — Connection latency | `fig08_connection_latency.py` | `results-v5/01_connection_latency/` |
| Figure 5 — Throughput vs loss | `fig09_throughput_vs_loss.py` | `results-v5/03_throughput_under_loss/` |
| Figure 6 — Datagram vs stream | `fig12_datagram_vs_stream.py` | `results-v5/05_datagram_vs_stream/` |
| Figure 7 — Strategy comparison | `fig14_strategy_comparison.py` | `results-v5/04_stream_strategies/` |
| Figure 8 — Frame packing ablation | `fig15_frame_packing.py` | `results-v5/02_hol_blocking/` |

All figure scripts are in `experiments/analysis/figures/` and share a common style defined in `style.py`.

---

## Experiments guide

### Standardized parameters (v5)

All experiments were run with these standardized parameters:

| Parameter | Value |
|-----------|-------|
| Hardware | GCP n2-standard-4 (4 vCPU, 16 GB RAM) |
| Region | us-west1-b (same availability zone) |
| OS | Debian 12, kernel 6.1 |
| Payload | 256 bytes |
| Duration | 60 seconds (Exp 1: 30 seconds) |
| Warmup | 5 seconds (Exp 1: none) |
| Runs per config | 15 |
| QUIC impl | Quinn 0.11 (Rust) |
| Congestion control | QUIC: NewReno, TCP: CUBIC |

### Infrastructure

Experiments run on GCP VMs organized into three parallel groups, each with a broker, publisher, and subscriber VM. Network impairment is applied symmetrically on the broker using `tc-netem`.

```
Group 1: broker-1 ←→ pub-1 / sub-1
Group 2: broker-2 ←→ pub-2 / sub-2
Group 3: broker-3 ←→ pub-3 / sub-3
```

**Setup sequence:**

1. Provision VMs: `cd setup && bash provision.sh provision`
2. Install dependencies: `bash install.sh`
3. Generate TLS certificates: `bash generate_bench_certs.sh`
4. Update `parallel/group{1,2,3}.env` with external IPs
5. Build binary on one VM, SCP to others
6. Run experiments via `parallel/run_all_v5.sh`

### Directory: `experiments/parallel/`

The primary experiment scripts. Each `*_v5.sh` script runs one experiment across all configurations.

| Script | Experiment | Configurations |
|--------|-----------|---------------|
| `01_connection_latency_v5.sh` | Exp 1 | 3 transports × 5 delays × 15 runs = 225 |
| `02_hol_blocking_v5.sh` | Exp 2 (main) | 4 transports × 4 loss rates × 15 runs = 240 |
| `02_hol_blocking_v5_fp.sh` | Exp 2 (frame packing) | 2 transports × 4 loss rates × 15 runs = 120 |
| `02b_hol_rtt_sweep_v5.sh` | Exp 2 (RTT sweep) | 4 transports × 3 RTTs × 15 runs = 180 |
| `02c_hol_topic_scaling_v5.sh` | Exp 2 (topic count) | 3 transports × 4 topic counts × 15 runs = 180 |
| `02d_hol_rtt_boundary_v5.sh` | Exp 2 (RTT boundary) | 4 transports × 2 RTTs × 15 runs = 120 |
| `02e_hol_qos1_v5.sh` | Exp 2 (QoS 1) | 2 transports × 1 condition × 15 runs = 30 |
| `03_throughput_under_loss_v5.sh` | Exp 3 | 4 transports × 5 losses × 2 QoS × 15 runs = 600 |
| `04_stream_strategies_v5.sh` | Exp 4 | 3 strategies × 4 topic counts × 2 modes × 15 runs = 360 |
| `05_datagram_vs_stream_v5.sh` | Exp 5 | 2 modes × 3 delays × 4 losses × 2 QoS × 15 runs = 720 |
| `06_resource_overhead_v5.sh` | Exp 6 | 4 transports × 3 concurrencies × 15 runs = 180 |

**Supporting files:**

- `common_parallel.sh` — SSH/SCP helpers, broker/client start/stop, monitor management, netem control. All scripts source this file.
- `group{1,2,3}.env` — Per-group VM IPs and SSH configuration. External IPs change on VM restart and must be updated manually.
- `run_all_v5.sh` — Master orchestrator that distributes experiments across groups and runs them in parallel. Total wall time ~21 hours.
- `run_group{1,2,3}_v5.sh` — Per-group execution scripts called by the orchestrator.

**How a single experiment run works:**

1. Apply network impairment on broker VM (`netem/apply.sh`)
2. Start broker with transport-specific flags
3. Start resource monitors on broker and client VMs
4. Run bench tool with configured parameters
5. Collect JSON results and CSV traces via SCP
6. Stop monitors, stop broker, clear network impairment
7. Repeat for next configuration

### Directory: `experiments/run/`

Legacy single-host scripts from Phase 1. Same experiments but designed for two-VM setups without parallel group coordination. Retained for reference. The `common.sh` file provides the framework these scripts use.

### Directory: `experiments/analysis/`

Post-processing and statistical analysis.

| Script | Purpose |
|--------|---------|
| `aggregate.py` | JSON result files to CSV summary tables |
| `hol_v3_analysis.py` | HOL blocking metrics: inter-topic spread, spike isolation ratio, windowed correlation |
| `hol_v4_frame_packing.py` | Greedy vs StreamIsolated frame packing comparison |
| `comprehensive_hol_analysis.py` | Cross-experiment HOL synthesis |
| `publication_stats.py` | Bootstrap 95% CI (10k resamples), Cliff's delta effect sizes, LaTeX tables |
| `verify_data.py` | Data integrity and completeness checks |

### Directory: `experiments/analysis/figures/`

Publication-quality figure generation using matplotlib.

| Script | Description |
|--------|------------|
| `fig01_spike_iso_vs_loss.py` | Spike isolation ratio vs packet loss rate |
| `fig02_spike_iso_vs_rtt.py` | Spike isolation ratio vs RTT |
| `fig03_spike_iso_vs_topics.py` | Spike isolation ratio vs topic count |
| `fig04_wcorr_vs_spike_iso.py` | Windowed correlation vs spike isolation scatter |
| `fig05_latency_percentiles.py` | p50/p95/p99 latency comparison |
| `fig06_timeseries_cwnd.py` | Congestion window time series |
| `fig07_inter_arrival.py` | Inter-arrival time clustering |
| `fig08_connection_latency.py` | Connection establishment latency bars |
| `fig09_throughput_vs_loss.py` | Throughput vs packet loss (QoS 0 + QoS 1) |
| `fig10_resource_overhead.py` | Resource overhead time series |
| `fig11_qos_comparison.py` | QoS 0 vs QoS 1 HOL comparison |
| `fig12_datagram_vs_stream.py` | Datagram vs stream latency + throughput |
| `fig13_resource_overhead_scaled.py` | Resource overhead across connection counts |
| `fig14_strategy_comparison.py` | Strategy latency + throughput vs topic count |
| `fig15_frame_packing.py` | Frame packing ablation (2x2 grid) |
| `generate_all.py` | Batch runner for all figures |
| `style.py` | Shared colors, labels, markers, matplotlib config |

**Generating all figures:**

```bash
cd experiments/analysis/figures
python3 generate_all.py ../../results-v5 output/
```

Output goes to `output/` as both PDF and PNG.

### Directory: `experiments/netem/`

Network impairment via Linux `tc-netem`. Interface is auto-detected from the default route.

- `apply.sh <delay_ms> <loss_pct>` — Apply symmetric delay and loss
- `clear.sh` — Remove all impairment rules

### Directory: `experiments/monitor/`

Procfs-based resource sampling at 1 Hz.

- `resource_monitor.sh <pid> [interval]` — Tracks RSS, CPU%, thread count, network I/O for a given process. Output: CSV with timestamped rows.
- `client_monitor.sh` — Client-side telemetry collection (same pattern).

### Directory: `experiments/setup/`

GCP VM provisioning and configuration.

- `provision.sh` — Terraform wrapper for VM lifecycle
- `install.sh` — Dependency installation (Rust toolchain, system packages)
- `generate_bench_certs.sh` — TLS/QUIC certificate generation with broker IP as SAN
- `config.env.example` — Template for GCP project and VM configuration

### Directory: `experiments/terraform/`

Infrastructure-as-code for GCP n2-standard-4 VMs. Outputs broker and client external IPs after provisioning.

### Results data

Experiment results are archived on Zenodo: [10.5281/zenodo.19098820](https://doi.org/10.5281/zenodo.19098820) (384 MB zip, 1.3 GB uncompressed). Download and extract into `experiments/results-v5/` to reproduce the figures. Each experiment subdirectory contains:

- **JSON files**: One per run, containing configuration, metrics, and summary statistics
- **CSV files**: Per-run trace data (message timestamps, Quinn QUIC stats, resource samples)

**v5 results summary (1.3 GB total):**

| Directory | JSON | CSV | Description |
|-----------|------|-----|-------------|
| `01_connection_latency/` | 225 | 467 | 3 transports, 5 delays |
| `02_hol_blocking/` | 360 | 1,440 | 4 transports, 4 loss rates + frame packing |
| `02b_hol_rtt_sweep/` | 180 | 719 | RTT sweep at 1% loss |
| `02c_hol_topic_scaling/` | 180 | 720 | 1-16 topics |
| `02d_hol_rtt_boundary/` | 120 | 480 | Fine-grained RTT (15, 20 ms) |
| `02e_hol_qos1/` | 30 | 120 | QoS 1 HOL comparison |
| `03_throughput_under_loss/` | 1,200 | 1,800 | 0-10% loss, QoS 0 + QoS 1 |
| `04_stream_strategies/` | 720 | 1,080 | 3 strategies, 4 topic counts |
| `05_datagram_vs_stream/` | 1,440 | 2,160 | Datagram vs stream, 3 RTTs |
| `06_resource_overhead/` | 360 | 540 | 10/50/100 connections |

Legacy results (`results-phase1/`, `results-phase2/`, `results-hol-v3/`, `results-hol-v4-framepacking/`) are also gitignored and retained for reference but are not used in the published paper.

### Reproducing the experiments

**Prerequisites:** GCP account with us-west1 quota for 9 n2-standard-4 VMs (3 groups of broker + pub + sub).

```bash
# 1. Provision infrastructure
cd experiments/setup
bash provision.sh provision
bash install.sh
bash generate_bench_certs.sh

# 2. Update VM IPs (external IPs change on restart)
#    Edit experiments/parallel/group{1,2,3}.env

# 3. Build and distribute binary
#    Build on one VM, SCP the release binary to all others

# 4. Run all experiments (~21 hours wall time)
cd experiments/parallel
bash run_all_v5.sh

# 5. Generate figures
cd experiments/analysis/figures
python3 generate_all.py ../../results-v5 output/

# 6. Teardown
cd experiments/setup
bash provision.sh teardown
```

### Reproducing figures from archived data

If you just want to regenerate the figures without running the experiments:

```bash
# 1. Download results from Zenodo
wget https://zenodo.org/records/19098820/files/mqtt-quic-experiment-results-v5.zip
unzip mqtt-quic-experiment-results-v5.zip -d experiments/

# 2. Generate figures
cd experiments/analysis/figures
python3 generate_all.py ../../results-v5 output/
```

### Key investigation documents

These markdown files document the experimental methodology evolution:

- `RESULTS_MANIFEST.md` — Complete inventory of all result files with counts and status
- `RESOURCE_REPORT.md` — Resource consumption analysis across experiments
- `HOL_BLOCKING_INVESTIGATION.md` — Root cause analysis of v1/v2 anomalies that led to the v3 redesign
- `INVESTIGATION_DIARY.md` — Chronological log of all investigation phases
- `PHASE2_PLAN.md` — Design document for Phase 2 experiments (not included in paper)
