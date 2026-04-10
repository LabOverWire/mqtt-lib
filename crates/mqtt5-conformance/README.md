# mqtt5-conformance

Vendor-neutral MQTT v5.0 conformance test suite that validates any broker against the OASIS specification.

## Features

- **183 conformance tests** covering MQTT v5.0 sections 1, 3, 4, and 6
- **247 normative statements** tracked in `conformance.toml` with per-statement coverage status
- **Two execution modes**: in-process via `cargo test` or against any external broker via the CLI
- **Capability-based skipping**: tests declare requirements; the runner skips tests the SUT cannot support
- **Raw TCP packet builder** for malformed and edge-case protocol testing
- **CLI with JSON reports** for CI integration and cross-vendor comparison

## Quick Start

Run conformance tests against the built-in in-process broker:

```bash
cargo test -p mqtt5-conformance --features inprocess-fixture --lib
```

Build the standalone CLI runner:

```bash
cargo build --release -p mqtt5-conformance-cli
```

Run against an external broker:

```bash
./target/release/mqtt5-conformance --sut tests/fixtures/mosquitto-2.x.toml
```

Generate a JSON conformance report:

```bash
./target/release/mqtt5-conformance --sut sut.toml --report report.json
```

## Testing Your Own Broker

1. **Create a SUT descriptor** — copy `tests/fixtures/external-mqtt5.toml` (the fully annotated reference template) and adjust addresses and capabilities to match your broker. See [BROKER_SETUP.md](BROKER_SETUP.md) for step-by-step configuration guides for Mosquitto, EMQX, and HiveMQ CE.

2. **Start your broker** — ensure it is listening on the addresses declared in the descriptor.

3. **Run the CLI**:
   ```bash
   ./target/release/mqtt5-conformance --sut your-broker.toml --report results.json
   ```

4. **Review results** — tests whose requirements exceed your broker's capabilities are skipped automatically. See the [CLI README](../mqtt5-conformance-cli/README.md) for the full SUT descriptor schema and report format.

## Test Organization

Tests are organized by MQTT v5.0 specification section. All 21 modules live in `src/conformance_tests/`:

| Module | Section | Scope |
|--------|---------|-------|
| `section1_data_repr` | 1 | Data representation, UTF-8 |
| `section3_connect` | 3.1 | CONNECT packet |
| `section3_connect_extended` | 3.1 | CONNECT edge cases, will messages, keep-alive |
| `section3_connack` | 3.2 | CONNACK packet |
| `section3_publish` | 3.3 | PUBLISH packet basics |
| `section3_publish_advanced` | 3.3 | PUBLISH properties and user properties |
| `section3_publish_alias` | 3.3 | Topic alias handling |
| `section3_publish_flow` | 3.3 | PUBLISH flow control |
| `section3_qos_ack` | 3.4-3.7 | PUBACK, PUBREC, PUBREL, PUBCOMP |
| `section3_subscribe` | 3.8-3.9 | SUBSCRIBE, SUBACK |
| `section3_unsubscribe` | 3.10-3.11 | UNSUBSCRIBE, UNSUBACK |
| `section3_ping` | 3.12-3.13 | PINGREQ, PINGRESP |
| `section3_disconnect` | 3.14 | DISCONNECT packet |
| `section3_final_conformance` | 3 | Cross-cutting packet rules |
| `section4_qos` | 4.3-4.4 | QoS delivery guarantees |
| `section4_flow_control` | 4.9 | Flow control |
| `section4_topic` | 4.7-4.8 | Topic names and filters |
| `section4_shared_sub` | 4.8.2 | Shared subscriptions |
| `section4_enhanced_auth` | 4.12 | Enhanced authentication |
| `section4_error_handling` | 4.13 | Error handling |
| `section6_websocket` | 6 | WebSocket conformance |

Additionally, 16 vendor-specific integration tests in `tests/` validate the conformance infrastructure itself.

## Writing Conformance Tests

Each test is an async function annotated with `#[conformance_test]`:

```rust
use mqtt5_conformance_macros::conformance_test;
use mqtt5_conformance::sut::SutHandle;

#[conformance_test(
    ids = ["MQTT-3.1.0-1"],
    requires = ["transport.tcp"],
)]
async fn connect_first_packet_must_be_connect(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::publish_without_connect())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.0-1] Server must disconnect client that sends non-CONNECT first"
    );
}
```

The function must be `async`, take exactly one parameter `sut: SutHandle`, and return `()`. See the [macros README](../mqtt5-conformance-macros/README.md) for parameter details, requirement string reference, and generated output.

## Requirement Strings

Tests declare capability requirements via the `requires` parameter. The runner skips tests whose requirements the SUT cannot satisfy.

| String | Description |
|--------|-------------|
| `transport.tcp` | TCP transport available |
| `transport.tls` | TLS transport available |
| `transport.websocket` | WebSocket transport available |
| `transport.quic` | QUIC transport available |
| `max_qos>=0` | QoS 0 supported (always true) |
| `max_qos>=1` | QoS 1 or higher supported |
| `max_qos>=2` | QoS 2 supported |
| `retain_available` | Retained messages supported |
| `wildcard_subscription_available` | Wildcard topic filters supported |
| `subscription_identifier_available` | Subscription identifiers supported |
| `shared_subscription_available` | Shared subscriptions supported |
| `enhanced_auth.<METHOD>` | Specific enhanced auth method supported |
| `hooks.restart` | Broker restart hook available |
| `hooks.cleanup` | Broker cleanup hook available |
| `acl` | Topic-level access control supported |

## Conformance Profiles

`profiles.toml` defines minimum capability sets for broker classes. Profiles are reference configurations — they document what a broker at a given tier must support, and can be used to validate that a SUT meets a profile's minimum bar.

| Profile | Description |
|---------|-------------|
| `core-broker` | Minimal spec-compliant broker. TCP only, no shared subscriptions, no ACL, no hooks. |
| `core-broker-tls` | Core broker with TLS transport enabled. |
| `full-broker` | Feature-complete broker. All transports except QUIC, shared subscriptions, ACL, SCRAM-SHA-256, lifecycle hooks. |

## Architecture

### SutHandle

The `SutHandle` enum abstracts over `InProcess` (in-process mqtt-lib broker) and `External` (any broker described by a TOML descriptor). Tests receive a `SutHandle` and never know which backend they run against.

### TestClient

`TestClient` provides a high-level API for conformance tests: connect, publish, subscribe, receive messages. It wraps either the mqtt5 client library (in-process) or a raw MQTT v5 protocol implementation (external). `Subscription` handles collect received messages and support timeout-based assertions.

### RawMqttClient and RawPacketBuilder

`RawMqttClient` operates at the TCP byte level, bypassing client-side validation. `RawPacketBuilder` constructs both valid and deliberately malformed MQTT v5.0 packets. Together they test broker rejection behavior for protocol violations.

### Registry

Tests self-register at link time via `linkme::distributed_slice`. The `CONFORMANCE_TESTS` slice collects all `ConformanceTest` entries across the workspace with zero runtime cost. The CLI iterates over this slice to discover and run tests.

### Capability Gating

Each `ConformanceTest` carries a `requires` array of `Requirement` values. Before running a test, the runner checks each requirement against the SUT's `Capabilities`. If any requirement fails, the test is skipped with a label identifying the missing capability.

## Crate Structure

| File | Purpose |
|------|---------|
| `lib.rs` | Public API surface and module exports |
| `sut.rs` | `SutDescriptor`, `SutHandle`, TOML parsing, address accessors |
| `capabilities.rs` | `Capabilities` struct, `Requirement` enum, `TransportSupport`, `EnhancedAuthSupport`, `HookSupport` |
| `registry.rs` | `ConformanceTest` record, `CONFORMANCE_TESTS` distributed slice, `TestRunner` type |
| `skip.rs` | `skip_if_missing()` evaluator, `SkipReason` |
| `test_client/` | `TestClient` enum, `Subscription`, `ReceivedMessage`, in-process and raw backends |
| `raw_client.rs` | `RawMqttClient` (byte-level transport), `RawPacketBuilder` (packet construction) |
| `transport.rs` | `Transport` trait, `TcpTransport` |
| `packet_parser.rs` | `ParsedProperties`, CONNACK/PUBLISH/SUBACK parsers, variable-length integer decoder |
| `assertions.rs` | Reusable assertion helpers for CONNACK reason codes, SUBACK codes, DISCONNECT, PUBLISH matching |
| `harness.rs` | `ConformanceBroker` — in-process broker fixture with random port binding |
| `manifest.rs` | `ConformanceManifest` — TOML-based statement tracking, coverage statistics |
| `report.rs` | `ConformanceReport` — text and JSON report generation |
| `conformance_tests/` | 21 test modules organized by specification section |
