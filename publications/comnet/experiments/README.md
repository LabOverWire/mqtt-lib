# MQoQ Benchmarking Infrastructure

Experiment infrastructure for measuring MQTT-over-QUIC performance characteristics
against TCP/TLS baselines using GCP Compute Engine instances managed by Terraform.

## Prerequisites

- [Terraform](https://developer.hashicorp.com/terraform/install) >= 1.5
- GCP project with Compute Engine API enabled
- `gcloud` CLI authenticated (`gcloud auth application-default login`)

## Quick Start

```bash
# 1. Configure
cp setup/config.env.example setup/config.env
# Edit config.env with your GCP project ID and repo URL

# 2. Set up Terraform variables
cp terraform/terraform.tfvars.example terraform/terraform.tfvars
# Edit terraform.tfvars with your GCP project ID

# 3. Provision instances
bash setup/provision.sh

# 4. Install dependencies and build
bash setup/install.sh

# 5. Generate TLS/QUIC certificates
bash setup/generate_bench_certs.sh $BROKER_IP

# 6. Run all experiments
bash run/run_all.sh

# 7. Aggregate results
python3 analysis/aggregate.py

# 8. Tear down
bash setup/provision.sh teardown
```

## Experiments

| Script | What it measures |
|--------|-----------------|
| `01_connection_latency.sh` | Connection setup time across TCP/TLS/QUIC with varying network delay |
| `02_hol_blocking.sh` | Head-of-line blocking: cross-topic latency correlation under packet loss |
| `02a_hol_traces_extended.sh` | Extended traces at 2% and 5% loss |
| `02b_hol_rtt_sweep.sh` | HOL blocking at varying RTTs (10/50/100ms) with 1% loss |
| `02c_hol_topic_scaling.sh` | HOL blocking at varying topic counts (2/4/16/32) |
| `02d_hol_rtt_boundary.sh` | RTT boundary experiment (15/20ms) to narrow isolation transition |
| `02e_hol_qos1.sh` | HOL blocking with QoS 1 at reference conditions |
| `03_throughput_under_loss.sh` | Message throughput degradation under packet loss for all transports |
| `04_stream_strategies.sh` | Comparison of QUIC stream strategies (control-only, per-publish, per-topic, per-subscription) |
| `05_datagram_vs_stream.sh` | QUIC datagrams vs streams for QoS 0 under varying loss |
| `06_resource_overhead.sh` | Memory, CPU, and thread usage under different connection counts |
| `12_payload_format_localhost.sh` | Payload format performance on localhost |
| `13_payload_format_remote.sh` | Payload format performance over GCP network |
| `14_payload_format_3host.sh` | Payload format with separate pub/broker/sub hosts |

### Script Status

**Complete** (results collected):
01, 02, 02a, 02b, 02c, 03, 04, 12

**Ready to run** (scripts written, need VM session):
02d, 02e, 13, 14

**Planned but not run** (scripts written, not yet validated):
05, 06

**Phase 2 / GKE** (Kubernetes pod-based, not run):
07, 08, 09, 10, 11

## Network Impairment

Uses `tc netem` on the client instance to simulate WAN conditions:
- `netem/apply.sh <delay_ms> <loss_pct>` - Apply delay and loss
- `netem/clear.sh` - Remove all impairments

The interface is auto-detected via the default route.

## Output Format

Each benchmark run produces a JSON file with:
- `mode` - benchmark type (throughput, latency, connections, hol-blocking)
- `config` - full parameter snapshot including transport metadata
- `results` - measured data with percentile statistics

The `analysis/aggregate.py` script computes mean, stdev, and 95% CI across repeated runs.

## Directory Structure

```
experiments/
├── terraform/       # GCP instance provisioning
├── setup/           # Config and installation
├── netem/           # Network impairment (tc netem)
├── run/             # Experiment scripts
├── monitor/         # Resource monitoring
├── analysis/        # Result aggregation
├── archive/         # Superseded results (see archive/ARCHIVE_README.md)
└── results/         # Raw JSON output (gitignored)
```
