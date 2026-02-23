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
| `03_throughput_under_loss.sh` | Message throughput degradation under packet loss for all transports |
| `04_stream_strategies.sh` | Comparison of QUIC stream strategies (control-only, per-publish, per-topic, per-subscription) |
| `05_datagram_vs_stream.sh` | QUIC datagrams vs streams for QoS 0 under varying loss |
| `06_resource_overhead.sh` | Memory, CPU, and thread usage under different connection counts |

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
└── results/         # Raw JSON output (gitignored)
```
