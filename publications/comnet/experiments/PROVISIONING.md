# GCP Provisioning Runbook

Provisioning and run procedure for the Computer Networks re-run experiments on the
`quic-experiments-488321` GCP project. Nothing here bills until Step 1 (starting VMs).

## Context

The bench project is **not** the active gcloud account, so every call needs explicit flags:

```bash
PROJECT=quic-experiments-488321
ACCOUNT=laboverwire@gmail.com
ZONE=us-west1-b
G="gcloud --project=$PROJECT --account=$ACCOUNT"
```

## Fleet

9 `n2-standard-4` VMs in `us-west1-b`, organized as 3 groups (group *N* = broker/pub/sub *N*).
Internal IPs are stable across stop/start; external IPs are reassigned on each start.

| Group | broker (internal) | pub | sub |
|---|---|---|---|
| 1 | `mqoq-broker-1` (10.138.0.21) | `mqoq-pub-1` | `mqoq-sub-1` |
| 2 | `mqoq-broker-2` (10.138.0.24) | `mqoq-pub-2` | `mqoq-sub-2` |
| 3 | `mqoq-broker-3` (10.138.0.27) | `mqoq-pub-3` | `mqoq-sub-3` (no external IP → ProxyJump via pub-3) |

An experiment script uses only **one** group (its broker/pub/sub); groups run different
experiments in parallel. So a single experiment needs only 3 VMs.

## Run order

1. **Exp 3 — throughput under loss** (contaminated by broker decay; single group).
2. **Exp 1 — connection latency** (the empty QUIC 0 ms cell).
3. Remaining (HOL family, Exp 5/6) per `run_all_v5.sh`.

---

## Exp 3 quick path (3 VMs, ~7.5 h, ~$4 of VM time)

`03_throughput_under_loss_v5.sh` runs qos(2) × loss(5) × 4 arms × 15 runs = 600 runs on group 1
only. Per-run broker restart (merged) eliminates the decay contamination.

### 1. Start group 1 (first billed action)

```bash
$G compute instances start mqoq-broker-1 mqoq-pub-1 mqoq-sub-1 --zone=$ZONE
```

### 2. Capture IPs

```bash
$G compute instances list --filter="name~mqoq-(broker|pub|sub)-1" \
  --format="table(name,networkInterfaces[0].networkIP,networkInterfaces[0].accessConfigs[0].natIP)"
```

### 3. Fill `parallel/group1.env`

`BROKER_IP` = broker-1 **internal** (10.138.0.21); `BROKER_SSH_IP` = broker-1 external;
`PUB_IP` = pub-1 external; `SUB_IP` = sub-1 external; `SSH_USER=bench`.

### 4. Mask snapd on broker-1 (SSH-drop fix)

```bash
ssh bench@<broker-1-ext> "sudo systemctl mask snapd snapd.socket; sudo systemctl stop snapd snapd.socket"
```

### 5. Refresh code + rebuild the stale binary (broker-1, pub-1, sub-1)

The VM binary predates C4/C5/C6 and #138. All three VMs run `mqttv5` (broker + bench client):

```bash
for h in <broker-1-ext> <pub-1-ext> <sub-1-ext>; do
  ssh bench@$h "cd /opt/mqtt-lib && git fetch origin && git checkout main && git pull \
    && source ~/.cargo/env && cargo build --release -p mqttv5-cli \
    && sudo ln -sf /opt/mqtt-lib/target/release/mqttv5 /usr/local/bin/mqttv5 && mqttv5 --version"
done
```

### 6. Regenerate certs on broker-1 (SAN = broker internal IP)

The bench connects to `quic://10.138.0.21:14567`, so the SAN must be the **internal** IP.
`setup/generate_bench_certs.sh` targets one broker + one client; run its openssl block on broker-1
with `BROKER_IP=10.138.0.21`, then copy `ca.pem` to pub-1 and sub-1.

### 7. Smoke test (before the full run)

```bash
GROUP=1 RUNS_PER_DATAPOINT=1 bash parallel/03_throughput_under_loss_v5.sh   # 1 run/cell, confirm non-empty JSON, no warn_if_empty
```

### 8. Full Exp 3

```bash
GROUP=1 bash parallel/03_throughput_under_loss_v5.sh 2>&1 | tee /tmp/exp3.log
```

Results land in `experiments/results-v5/03_throughput_under_loss/`. Watch the log for
`WARN: empty result` / `WARN: broker restart failed` / `WARN: failed to restore netem`.

### 9. Teardown (stop billing)

```bash
$G compute instances stop mqoq-broker-1 mqoq-pub-1 mqoq-sub-1 --zone=$ZONE
```

---

## Full 9-VM suite (later)

For the complete parallel suite, start all 9 VMs, provision each (steps 3–6 per group, filling
`group{1,2,3}.env`), and launch `run_all_v5.sh` (3 groups in parallel, ~21 h). `setup/install.sh`
and `setup/generate_bench_certs.sh` target the 2-VM layout, so loop them per group.

## Notes

- Never use `--insecure`; always `--ca-cert /opt/mqtt-certs/ca.pem`.
- `group3.env` uses `SUB_IP`=internal + `SUB_PROXY`=pub-3 external (sub-3 has no external IP).
- Disks are 50 GB and persist across stop/start — do **not** delete them (the machine image is gone;
  the disks are the only recreation path).
