# Broker Setup Guide for Conformance Testing

The conformance suite ships with fixture TOML files that describe each broker's address, capabilities, and behavior. This guide shows how to configure Mosquitto, EMQX, and HiveMQ CE to match those fixtures so the test runner produces accurate results.

## General Requirements

Every broker must have these features enabled:

- MQTT v5.0 protocol support
- QoS 0, 1, and 2
- Retained messages
- Wildcard subscriptions (`+` and `#`)
- Subscription identifiers
- Assigned client IDs (server assigns an ID when the client sends an empty string)

### Test Coverage by Feature

Most tests require only a TCP listener. The counts below show how many tests gate on each capability, so you can prioritize which features to configure first:

| Capability | Tests |
|------------|-------|
| TCP only (core protocol) | ~118 |
| QoS 1 | ~21 |
| QoS 2 | ~19 |
| Retained messages | ~13 |
| Shared subscriptions | ~11 |
| WebSocket | 3 |

Total: **183 conformance tests**.

## Mosquitto 2.x

**Fixture file:** `tests/fixtures/mosquitto-2.x.toml`

### Installation

```bash
# Debian/Ubuntu
sudo apt install mosquitto

# macOS
brew install mosquitto
```

### Configuration

Create or edit `mosquitto.conf`:

```
listener 1883
protocol mqtt

listener 8883
protocol mqtt
certfile /path/to/server.pem
keyfile /path/to/server.key
cafile /path/to/ca.pem

listener 8080
protocol websockets

allow_anonymous true
max_topic_alias 10
max_inflight_messages 20
```

Key values that match the fixture:

| Fixture field | Value | Mosquitto setting |
|---------------|-------|-------------------|
| `topic_alias_maximum` | 10 | `max_topic_alias 10` |
| `server_receive_maximum` | 20 | `max_inflight_messages 20` |
| `maximum_packet_size` | 268435456 | Mosquitto default (256 MB) |

### Start

```bash
mosquitto -c /path/to/mosquitto.conf
```

### Expected Skips

Mosquitto's default config does not support enhanced authentication or ACL, so tests requiring those capabilities are skipped automatically.

### Run

```bash
./target/release/mqtt5-conformance --sut tests/fixtures/mosquitto-2.x.toml
```

## EMQX 5.x

**Fixture file:** `tests/fixtures/emqx.toml`

### Installation (Docker)

```bash
docker run -d --name emqx \
  -p 1883:1883 \
  -p 8083:8083 \
  -p 8883:8883 \
  -p 18083:18083 \
  emqx/emqx:5.8
```

Port 18083 is the EMQX Dashboard (HTTP API) used for configuration.

### Key Configuration

EMQX defaults work for most settings. Verify or adjust via the Dashboard or `emqx.conf`:

| Fixture field | Value | Notes |
|---------------|-------|-------|
| `topic_alias_maximum` | 65535 | EMQX default |
| `server_receive_maximum` | 32 | EMQX default |
| `maximum_packet_size` | 1048576 | EMQX default is 1 MB (not 256 MB) |
| WebSocket port | 8083 | EMQX default (not the common 8080) |

#### Enhanced Auth (SCRAM-SHA-256)

The fixture declares `methods = ["SCRAM-SHA-256", "SCRAM-SHA-512"]`. To enable SCRAM authentication, configure it through the EMQX Dashboard under **Authentication** → **SCRAM** or via the REST API.

#### ACL

The fixture sets `acl = true`. Configure publish/subscribe ACL rules through the Dashboard under **Authorization** or via `emqx.conf`.

### Expected Capabilities

EMQX supports nearly the full conformance profile: TCP, TLS, WebSocket, QUIC, shared subscriptions, enhanced auth, and ACL.

### Run

```bash
./target/release/mqtt5-conformance --sut tests/fixtures/emqx.toml
```

## HiveMQ CE

**Fixture file:** `tests/fixtures/hivemq-ce.toml`

### Installation

**Docker:**

```bash
docker run -d --name hivemq \
  -p 1883:1883 \
  -p 8000:8000 \
  hivemq/hivemq-ce:latest
```

**Zip download:** extract and run `bin/run.sh` from the [HiveMQ CE releases](https://github.com/hivemq/hivemq-community-edition/releases).

### Configuration

Edit `conf/config.xml` to add a WebSocket listener:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<hivemq>
    <listeners>
        <tcp-listener>
            <port>1883</port>
            <bind-address>0.0.0.0</bind-address>
        </tcp-listener>
        <websocket-listener>
            <port>8000</port>
            <bind-address>0.0.0.0</bind-address>
            <path>/mqtt</path>
        </websocket-listener>
    </listeners>
</hivemq>
```

Key values that match the fixture:

| Fixture field | Value | Notes |
|---------------|-------|-------|
| `topic_alias_maximum` | 5 | HiveMQ CE default |
| `server_receive_maximum` | 10 | HiveMQ CE default |
| `maximum_packet_size` | 268435456 | HiveMQ CE default (256 MB) |
| WebSocket port | 8000 | Not the common 8080 |
| TLS | not available | Enterprise Edition only |

### Expected Skips

HiveMQ CE does not support TLS (Enterprise only), enhanced authentication, or ACL. Tests requiring those capabilities are skipped automatically.

### Run

```bash
./target/release/mqtt5-conformance --sut tests/fixtures/hivemq-ce.toml
```

## mqtt-lib (External Process)

**Fixture file:** `tests/fixtures/external-mqtt5.toml`

This runs the mqtt-lib broker as a standalone process and tests it over real TCP, rather than the in-process mode used by `cargo test`.

### Build

```bash
cargo build --release -p mqttv5-cli
```

### Start

```bash
./target/release/mqttv5 broker --host 0.0.0.0:1883
```

### Run

```bash
./target/release/mqtt5-conformance --sut tests/fixtures/external-mqtt5.toml
```

## Custom Broker Setup

To test a broker not listed here:

1. Copy `tests/fixtures/external-mqtt5.toml` — it is the fully annotated reference template with every field documented.
2. Adjust addresses and capabilities to match your broker's actual configuration.
3. Start your broker and run:
   ```bash
   ./target/release/mqtt5-conformance --sut your-broker.toml
   ```

Tests whose requirements exceed your broker's declared capabilities are skipped automatically.

## Further Reading

- [Conformance suite README](README.md) — test organization, requirement strings, writing new tests
- [CLI README](../mqtt5-conformance-cli/README.md) — full SUT descriptor schema, report format, CI integration
- [`external-mqtt5.toml`](tests/fixtures/external-mqtt5.toml) — annotated reference template for custom descriptors
