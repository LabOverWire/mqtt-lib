# MQTT v5.0 Conformance Test Suite — Implementation Diary

## Planned Work

- [x] Section 3.1 — CONNECT (21 tests, 23 normative statements)
- [ ] Section 3.2 — CONNACK (session present, reason codes, properties)
- [x] Section 3.3 — PUBLISH (22 tests, 43 normative statements)
- [ ] Section 3.4 — PUBACK (reason codes, QoS 1 acknowledgement)
- [ ] Section 3.5 — PUBREC (QoS 2 first phase)
- [ ] Section 3.6 — PUBREL (QoS 2 second phase)
- [ ] Section 3.7 — PUBCOMP (QoS 2 completion)
- [ ] Section 3.8 — SUBSCRIBE (options, wildcards, shared subscriptions)
- [ ] Section 3.9 — SUBACK (reason codes, subscription confirmation)
- [ ] Section 3.10 — UNSUBSCRIBE (acknowledgement, topic filter matching)
- [ ] Section 3.11 — UNSUBACK (reason codes)
- [ ] Section 3.12 — PINGREQ/PINGRESP (keep-alive enforcement)
- [ ] Section 3.14 — DISCONNECT (reason codes, session expiry override, will suppression)

**Rule**: after every step, every detail learned, every fix applied — add an entry here. New entries go on top, beneath this plan list.

---

## Diary Entries

### 2026-02-16 — Section 3.3 PUBLISH complete

22 passing tests across 7 groups:

- **Group 1 — Malformed/Invalid PUBLISH** (6 raw-client tests): QoS=3, DUP+QoS0, wildcard topic, empty topic, topic alias zero, subscription identifier from client
- **Group 2 — Retained Messages** (3 tests): retain stores, empty payload clears, non-retain doesn't store
- **Group 3 — Retain Handling Options** (3 tests): SendAtSubscribe, SendIfNew, DontSend
- **Group 4 — Retain As Published** (2 tests): retain_as_published=false clears flag, =true preserves flag
- **Group 5 — QoS Response Flows** (3 tests): QoS 0 delivery, QoS 1 PUBACK, QoS 2 full flow
- **Group 6 — Property Forwarding** (4 tests): PFI, content type, response topic + correlation data, user properties order
- **Group 7 — Topic Matching** (1 test): wildcard subscription receives correct topic name

Two broker conformance gaps discovered and fixed:
1. DUP=1 with QoS=0 was not rejected — added validation in `publish.rs` decode path `[MQTT-3.3.1-2]`
2. Subscription Identifier in client-to-server PUBLISH was not rejected — added check in `handle_publish` `[MQTT-3.3.4-6]`

Added `RawPacketBuilder` methods: `publish_qos0`, `publish_qos1`, `publish_qos3_malformed`, `publish_dup_qos0`, `publish_with_wildcard_topic`, `publish_with_empty_topic`, `publish_with_topic_alias_zero`, `publish_with_subscription_id`.

Added `RawMqttClient` methods: `connect_and_establish`, `expect_puback`.

43 normative statements tracked in `conformance.toml` Section 3.3: 22 Tested, 14 Untested, 4 NotApplicable, 3 Untested (topic alias lifecycle).

### 2025-02-16 — Section 3.1 CONNECT complete

Crate structure created and fully operational:

- `src/harness.rs` — `ConformanceBroker` (in-process, memory-backed, random port), `MessageCollector`, helper functions
- `src/raw_client.rs` — `RawMqttClient` (raw TCP) + `RawPacketBuilder` (hand-crafted malformed packets)
- `src/manifest.rs` — `ConformanceManifest` deserialized from `conformance.toml`, coverage metrics
- `src/report.rs` — text and JSON conformance report generation
- `conformance.toml` — 23 normative statements from Section 3.1 tracked
- `tests/section3_connect.rs` — 21 passing tests

Tests cover: first-packet-must-be-CONNECT, second-CONNECT-is-protocol-error, protocol name/version validation, reserved flag, clean start session handling, will flag/QoS/retain semantics, client ID assignment, malformed packet handling, duplicate properties, fixed header flags, password-without-username, will publish on abnormal disconnect, will suppression on normal disconnect.

Three real broker conformance gaps discovered and fixed during implementation:
1. Fixed header flags validation was missing — added `validate_flags()` in `mqtt5-protocol/src/packet.rs`
2. Will QoS must be 0 when Will Flag is 0 — added validation in `connect.rs`
3. Will Retain must be 0 when Will Flag is 0 — added validation in `connect.rs`

Clippy pedantic clean. All doc comments use `///` with backtick-quoted MQTT terms (`CleanStart`, `QoS`, `ClientID`).
