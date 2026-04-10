# mqtt5-conformance-cli

Standalone CLI runner for the MQTT v5.0 conformance test suite. Runs 183 conformance tests against any MQTT v5.0 broker described by a TOML descriptor file.

## Installation

```bash
cargo build --release -p mqtt5-conformance-cli
# Binary: ./target/release/mqtt5-conformance
```

## Usage

Run against the built-in in-process broker:

```bash
./target/release/mqtt5-conformance
```

Run against an external broker:

```bash
./target/release/mqtt5-conformance --sut tests/fixtures/mosquitto-2.x.toml
```

Generate a JSON conformance report:

```bash
./target/release/mqtt5-conformance --sut sut.toml --report report.json
```

Filter tests by name substring:

```bash
./target/release/mqtt5-conformance connect
```

List all tests without running them:

```bash
./target/release/mqtt5-conformance --list
```

## CLI Arguments

| Argument | Description |
|----------|-------------|
| `--sut <path>` | Path to a TOML file describing the System Under Test. If omitted, uses the built-in in-process mqtt-lib broker with WebSocket support. |
| `--report <path>` | Write a JSON conformance report to this path after the test run completes. |
| All other args | Forwarded to the `libtest_mimic` test harness. Supports `--list`, `--ignored`, `--nocapture`, name substring filters, and all standard test flags. |

## SUT Descriptor Format

The SUT descriptor is a TOML file that tells the runner how to reach the broker and what it supports. See `tests/fixtures/external-mqtt5.toml` for a fully annotated reference template, and [BROKER_SETUP.md](../mqtt5-conformance/BROKER_SETUP.md) for step-by-step broker configuration guides.

```toml
# Human-readable broker identifier shown in reports
name = "my-broker"

# TCP address (scheme: "mqtt" or bare host:port)
address = "mqtt://127.0.0.1:1883"

# TLS address (scheme: "mqtts"); empty string = not available
tls_address = "mqtts://127.0.0.1:8883"

# WebSocket address (scheme: "ws" or "wss"); empty string = not available
ws_address = "ws://127.0.0.1:8080/mqtt"

[capabilities]
# Spec-advertised values (from CONNACK properties)
max_qos = 2                              # u8: 0, 1, or 2 [default: 2]
retain_available = true                   # bool [default: true]
wildcard_subscription_available = true    # bool [default: true]
subscription_identifier_available = true  # bool [default: true]
shared_subscription_available = true      # bool [default: true]
topic_alias_maximum = 65535              # u16 [default: 65535]
server_keep_alive = 0                    # u16 [default: 0]
server_receive_maximum = 65535           # u16 [default: 65535]
maximum_packet_size = 268435456          # u32 [default: 268435456]
assigned_client_id_supported = true       # bool [default: true]

# Broker-imposed limits
max_clients = 0                           # u32, 0 = unlimited [default: 0]
max_subscriptions_per_client = 0          # u32, 0 = unlimited [default: 0]
session_expiry_interval_max_secs = 4294967295  # u32 [default: u32::MAX]
enforces_inbound_receive_maximum = true   # bool [default: true]

# Broker-specific behaviors
injected_user_properties = []             # string[] [default: []]
auth_failure_uses_disconnect = false      # bool [default: false]
unsupported_property_behavior = "DisconnectMalformed"  # string [default: "DisconnectMalformed"]
shared_subscription_distribution = "RoundRobin"        # string [default: "RoundRobin"]

# Access control
acl = false                               # bool [default: false]

[capabilities.transports]
tcp = true          # bool [default: true]
tls = false         # bool [default: false]
websocket = false   # bool [default: false]
quic = false        # bool [default: false]

[capabilities.enhanced_auth]
methods = []        # string[] of supported auth methods [default: []]

[capabilities.hooks]
restart = false     # bool: broker can be restarted between tests [default: false]
cleanup = false     # bool: broker state can be cleaned up [default: false]

[hooks]
restart_command = ""   # shell command to restart the broker
cleanup_command = ""   # shell command to clean up broker state
```

Fields not present in the TOML file use their defaults. Most defaults are permissive (max values, features enabled), so you only need to declare fields where your broker diverges from the protocol maximums.

## Capability-Based Skipping

Each conformance test declares its requirements via `requires = [...]` in the `#[conformance_test]` attribute. At startup, the CLI evaluates every test's requirements against the SUT's declared capabilities:

- **All requirements met** — test runs normally
- **Any requirement unmet** — test is marked as "ignored" with a label showing the missing capability (e.g., `[missing: transport.tls]`)

Ignored tests appear in the report but do not count as failures. This allows the same test suite to validate brokers with different feature sets without false negatives.

## Report Format

When `--report <path>` is provided, the CLI writes a JSON file with this structure:

```json
{
  "summary": {
    "passed": 175,
    "failed": 0,
    "ignored": 8,
    "filtered_out": 0,
    "measured": 0
  },
  "capabilities": {
    "max_qos": 2,
    "retain_available": true,
    "wildcard_subscription_available": true,
    "subscription_identifier_available": true,
    "shared_subscription_available": true,
    "injected_user_properties": ["x-mqtt-sender", "x-mqtt-client-id"]
  },
  "tests": [
    {
      "name": "connect_first_packet_must_be_connect",
      "module_path": "mqtt5_conformance::conformance_tests::section3_connect",
      "file": "src/conformance_tests/section3_connect.rs",
      "line": 42,
      "ids": ["MQTT-3.1.0-1"],
      "status": "planned",
      "unmet_requirement": null
    },
    {
      "name": "websocket_binary_frames",
      "module_path": "mqtt5_conformance::conformance_tests::section6_websocket",
      "file": "src/conformance_tests/section6_websocket.rs",
      "line": 10,
      "ids": ["MQTT-6.0.0-1"],
      "status": "skipped",
      "unmet_requirement": "transport.websocket"
    }
  ]
}
```

| Field | Description |
|-------|-------------|
| `summary` | Aggregate pass/fail/ignore/filter counts from the test run |
| `capabilities` | Snapshot of the SUT's declared capabilities |
| `tests` | Per-test entries with conformance IDs, source location, and skip status |
| `status` | `"planned"` (ran or will run) or `"skipped"` (requirement unmet) |
| `unmet_requirement` | The first unmet requirement string, or `null` if all requirements met |

## CI Integration

```bash
cargo build --release -p mqtt5-conformance-cli
./target/release/mqtt5-conformance --sut sut.toml --report conformance.json
```

The CLI exits with code 0 if all non-skipped tests pass, nonzero otherwise.
