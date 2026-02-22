# mqtt5-conformance

MQTT v5.0 OASIS specification conformance test suite for the `mqtt5` broker.

## Overview

Tests every normative statement (`[MQTT-x.x.x-y]`) from the
[OASIS MQTT Version 5.0 specification](https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html)
against the real broker running in-process on loopback TCP.

- **247 normative statements** tracked in `conformance.toml`
- **197 tests** across 22 test files covering sections 1, 3, 4, and 6
- Raw TCP packet builder for malformed/edge-case testing
- Machine-readable coverage reports (text and JSON)

## Running

```bash
cargo test -p mqtt5-conformance
```

## Architecture

### Test Harness

`ConformanceBroker` spins up a real `MqttBroker` with memory-backed storage on a random loopback port. Each test gets its own isolated broker instance.

### Raw Client

`RawMqttClient` operates at the TCP byte level, bypassing the normal client's packet validation. `RawPacketBuilder` constructs both valid and deliberately invalid MQTT v5.0 packets for testing broker rejection behavior.

### Conformance Manifest

`conformance.toml` maps every normative statement to its test status (`Tested`, `NotApplicable`, `Untested`) and associated test names. The `report` module generates coverage reports from this manifest.

## Test Organization

| File | Section | Scope |
|------|---------|-------|
| `section1_data_repr` | 1 | Data representation, UTF-8 |
| `section3_connect` | 3.1 | CONNECT packet |
| `section3_connect_extended` | 3.1 | CONNECT edge cases |
| `section3_connack` | 3.2 | CONNACK packet |
| `section3_publish` | 3.3 | PUBLISH packet |
| `section3_publish_advanced` | 3.3 | PUBLISH edge cases |
| `section3_publish_alias` | 3.3 | Topic aliases |
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
