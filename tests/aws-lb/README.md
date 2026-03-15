# AWS Load Balancer Integration Tests

End-to-end tests validating the MQTT broker's load balancer mode across TCP, TLS, and QUIC transports on real AWS infrastructure.

## Architecture

```
                    ┌──────────────┐
                    │    Client    │
                    │  (local CLI) │
                    └──────┬───────┘
                           │
                    1. CONNECT
                           │
                    ┌──────▼───────┐
                    │   LB Broker  │
                    │   (VM: lb)   │
                    └──────┬───────┘
                           │
              2. CONNACK UseAnotherServer (0x9C)
                 + ServerReference = backend URL
                           │
                    ┌──────▼───────┐
                    │    Client    │
                    │  follows     │
                    │  redirect    │
                    └──────┬───────┘
                           │
              3. CONNECT to selected backend
                           │
            ┌──────────────┼──────────────┐
            │                             │
     ┌──────▼───────┐             ┌──────▼───────┐
     │  Backend A   │             │  Backend B   │
     │   (VM: a)    │             │   (VM: b)    │
     └──────────────┘             └──────────────┘
```

The LB broker never handles MQTT traffic. It only redirects clients to a backend selected by hashing the client ID. After redirect, the client connects directly to the backend.

## How the Load Balancer Works

1. Client sends CONNECT to the LB broker
2. LB hashes the client ID (byte-sum modulo backend count) to select a backend
3. LB responds with CONNACK reason code `UseAnotherServer` (0x9C) and a `ServerReference` property containing the backend URL
4. Client parses the `ServerReference` URL and reconnects to the backend
5. Client is now connected directly to the backend — no traffic flows through the LB

The redirect is transport-agnostic: the URL scheme in `ServerReference` determines the transport the client uses for the backend connection (`mqtt://` for TCP, `mqtts://` for TLS, `quic://` for QUIC).

### Transport-Specific Behavior

- **TCP**: Redirect URL is `mqtt://ip:1883`. No certificate validation.
- **TLS**: Redirect URL is `mqtts://ip:8883`. Client must trust the CA that signed both the LB cert and the backend cert. Each instance has its own certificate with SAN matching its IP.
- **QUIC**: Redirect URL is `quic://ip:14567`. Same CA trust requirement as TLS. QUIC handshake adds startup latency (~5s in tests).

## Infrastructure

Three EC2 instances provisioned via OpenTofu in [`laboverwire/infrastructure/mqtt-lb-tests/`](../../../laboverwire/infrastructure/mqtt-lb-tests/):

| Instance | Role | Ports |
|----------|------|-------|
| `mqtt-lb` | Load balancer broker | 1883, 8883, 14567 |
| `mqtt-backend-a` | Backend broker A | 1883, 8883, 14567 |
| `mqtt-backend-b` | Backend broker B | 1883, 8883, 14567 |

- Instance type: `t3.medium` (Ubuntu 24.04)
- Region: `ca-west-1`
- Each instance gets an Elastic IP (static address)
- User data script clones the `server-redirect` branch, builds the CLI, and writes `/tmp/build-done` marker

### Cost Estimate

3x `t3.medium` at ~$0.0416/hr each = **~$0.125/hr** total. Including Elastic IPs and storage: **~$0.15/hr**.

## Setup Process

`setup.sh` performs the following steps:

1. **Wait for builds** — polls `/tmp/build-done` on each instance (up to 10 minutes)
2. **Verify binaries** — runs `mqttv5 --version` on each instance
3. **Generate TLS certificates** — creates a local CA and per-instance certificates with IP SANs:
   - CA key + cert (RSA 2048, 30-day validity)
   - Per-instance EC key (prime256v1) + cert signed by CA
4. **Distribute certificates** — SCPs CA cert + server cert/key to each instance at `/opt/mqtt-certs/`
5. **Generate LB configs** — creates three JSON config files:
   - `lb-tcp.json` — backends on port 1883 (`mqtt://` URLs)
   - `lb-tls.json` — backends on port 8883 (`mqtts://` URLs), TLS enabled on LB
   - `lb-quic.json` — backends on port 14567 (`quic://` URLs), QUIC enabled on LB
6. **Deploy configs** — SCPs config files to the LB instance

## Test Matrix

| # | Transport | Test | What It Validates |
|---|-----------|------|-------------------|
| 1 | TCP | pub through LB | Client follows redirect and publishes |
| 2 | TCP | sub through LB receives | Subscriber follows redirect, receives from both backends |
| 3 | TCP | pub+sub end-to-end | Subscribers on separate backends both receive via LB |
| 4 | TCP | distribution across backends | Multiple client IDs hash to different backends |
| 5 | TLS | pub through LB | TLS redirect with CA-validated cert chain |
| 6 | TLS | sub through LB receives | TLS subscriber redirect and message receipt |
| 7 | TLS | pub+sub end-to-end | Full TLS flow with cross-backend delivery |
| 8 | TLS | wrong CA cert rejected | Connection without correct CA fails |
| 9 | QUIC | pub through LB | QUIC redirect with CA-validated cert chain |
| 10 | QUIC | sub through LB receives | QUIC subscriber redirect and message receipt |
| 11 | QUIC | pub+sub end-to-end | Full QUIC flow with cross-backend delivery |
| 12 | QUIC | dead backend error | Redirect to stopped backends produces error |

## How to Run

### 1. Provision Infrastructure

```bash
cd /path/to/laboverwire/infrastructure/mqtt-lb-tests
tofu init
tofu apply
```

Wait for `tofu apply` to complete. The user data scripts will clone the repo and build the binary on each instance (~5-8 minutes after provisioning).

### 2. Setup

```bash
cd tests/aws-lb
bash setup.sh
```

This waits for builds, generates/distributes certs, and deploys LB configs.

### 3. Run Tests

```bash
bash run_tests.sh
```

The script starts/stops brokers as needed between transport groups and reports pass/fail counts.

### 4. Teardown

```bash
bash teardown.sh           # Stop brokers, clean local certs
bash teardown.sh --destroy  # Also destroy AWS infrastructure
```

## Files

| File | Purpose |
|------|---------|
| `env.sh` | Shared environment: IPs from OpenTofu output, SSH helpers |
| `setup.sh` | Build verification, cert generation, config deployment |
| `run_tests.sh` | Test runner with TCP/TLS/QUIC test groups |
| `teardown.sh` | Cleanup brokers and optionally destroy infrastructure |
