# MQTT v5.0 Conformance Test Suite — Implementation Diary

## Planned Work

- [x] Section 3.1 — CONNECT (21 tests, 23 normative statements)
- [x] Section 3.2 — CONNACK (11 tests, 22 normative statements)
- [x] Section 3.3 — PUBLISH (22 tests, 43 normative statements)
- [x] Section 3.4 — PUBACK (2 tests, 2 normative statements)
- [x] Section 3.5 — PUBREC (2 tests, 2 normative statements)
- [x] Section 3.6 — PUBREL (2 tests, 3 normative statements)
- [x] Section 3.7 — PUBCOMP (2 tests, 2 normative statements)
- [x] Section 3.8 — SUBSCRIBE (4 tests, 4 normative statements)
- [x] Section 3.9 — SUBACK (8 tests, 4 normative statements)
- [x] Section 3.10 — UNSUBSCRIBE (2 tests, 3 normative statements)
- [x] Section 3.11 — UNSUBACK (7 tests, 2 normative statements)
- [x] Section 3.12 — PINGREQ (5 tests, 1 normative statement)
- [x] Section 3.13 — PINGRESP (0 normative statements, covered by 3.12 tests)
- [ ] Section 3.14 — DISCONNECT (reason codes, session expiry override, will suppression)

**Rule**: after every step, every detail learned, every fix applied — add an entry here. New entries go on top, beneath this plan list.

---

## Diary Entries

### 2026-02-17 — Sections 3.12–3.13 PINGREQ/PINGRESP complete

5 passing tests across 2 groups in `section3_ping.rs`:

- **Group 1 — PINGREQ/PINGRESP Exchange** (2 tests): single PINGREQ gets PINGRESP `[MQTT-3.12.4-1]`, 3 sequential PINGREQs all get PINGRESPs
- **Group 2 — Keep-Alive Timeout Enforcement** (3 tests): keep-alive=2s timeout closes connection within 1.5x `[MQTT-3.1.2-11]`, keep-alive=0 disables timeout (connection survives 5s silence), PINGREQ resets keep-alive timer (5s of pings at 1s intervals keeps 2s keep-alive alive)

No broker conformance gaps discovered — PINGREQ handler and keep-alive enforcement both work correctly.

Added `RawMqttClient` methods: `expect_pingresp`.
Added `RawPacketBuilder` methods: `pingreq`, `connect_with_keepalive`.

Updated `MQTT-3.1.2-11` from Untested to Tested.
1 normative statement tracked in `conformance.toml` Section 3.12: Tested. Section 3.13 has no normative MUST statements.

### 2026-02-17 — Sections 3.10–3.11 UNSUBSCRIBE/UNSUBACK complete

9 passing tests across 3 groups in `section3_unsubscribe.rs`:

- **Group 1 — UNSUBSCRIBE Structure** (2 tests): invalid flags rejected `[MQTT-3.10.1-1]`, empty payload rejected `[MQTT-3.10.3-2]`
- **Group 2 — UNSUBACK Response** (4 tests): packet ID matches `[MQTT-3.11.2-1]`, one reason code per filter `[MQTT-3.11.3-1]`, Success for existing subscription, NoSubscriptionExisted (0x11) for non-existent
- **Group 3 — Subscription Removal Verification** (3 tests): unsubscribe stops delivery `[MQTT-3.10.4-1]`, partial multi-filter unsubscribe with mixed reason codes, idempotent unsubscribe (first=Success, second=NoSubscriptionExisted)

One broker conformance gap discovered and fixed:
1. WASM broker `handle_unsubscribe` always returned `UnsubAckReasonCode::Success` regardless of whether a subscription existed — fixed to capture `router.unsubscribe()` return value and use `NoSubscriptionExisted` (0x11) when `removed == false`, matching the native broker pattern. Also made session update conditional on `removed == true`.

Added `RawMqttClient` methods: `expect_unsuback`.
Added `RawPacketBuilder` methods: `unsubscribe`, `unsubscribe_multiple`, `unsubscribe_invalid_flags`, `unsubscribe_empty_payload`.

5 normative statements tracked in `conformance.toml` Sections 3.10–3.11: all Tested.

### 2026-02-17 — Sections 3.8–3.9 SUBSCRIBE/SUBACK complete

12 passing tests across 5 groups in `section3_subscribe.rs`:

- **Group 1 — SUBSCRIBE Structure** (3 tests): invalid flags rejected `[MQTT-3.8.1-1]`, empty payload rejected `[MQTT-3.8.3-3]`, NoLocal on shared subscription rejected `[MQTT-3.8.3-4]`
- **Group 2 — SUBACK Response** (3 tests): packet ID matches `[MQTT-3.9.2-1]`, one reason code per filter `[MQTT-3.9.3-1]`, reason codes in order with mixed auth `[MQTT-3.9.3-2]`
- **Group 3 — QoS Granting** (3 tests): grants exact requested QoS `[MQTT-3.9.3-3]`, downgrades to max QoS, message delivery at granted QoS
- **Group 4 — Authorization & Quota** (2 tests): NotAuthorized (0x87) via ACL denial, QuotaExceeded (0x97) via max_subscriptions_per_client
- **Group 5 — Subscription Replacement** (1 test): second subscribe to same topic replaces first, only one message copy delivered

One broker conformance gap discovered and fixed:
1. NoLocal=1 on shared subscriptions (`$share/group/topic`) was not rejected — added validation in `subscribe.rs` for both native and WASM brokers, sending DISCONNECT with ProtocolError (0x82) `[MQTT-3.8.3-4]`

Added `RawMqttClient` methods: `expect_suback`, `expect_publish`.
Added `RawPacketBuilder` methods: `subscribe_with_packet_id`, `subscribe_multiple`, `subscribe_invalid_flags`, `subscribe_empty_payload`, `subscribe_shared_no_local`.

8 normative statements tracked in `conformance.toml` Sections 3.8–3.9: all Tested.

### 2026-02-17 — Sections 3.4–3.7 QoS Ack packets complete

9 passing tests across 5 groups in `section3_qos_ack.rs`:

- **Group 1 — PUBACK (Section 3.4)** (2 tests): correct packet ID + reason code, message delivery on QoS 1
- **Group 2 — PUBREC (Section 3.5)** (2 tests): correct packet ID + reason code, no delivery before PUBREL
- **Group 3 — PUBREL (Section 3.6)** (2 tests): invalid flags rejected `[MQTT-3.6.1-1]`, unknown packet ID returns `PacketIdentifierNotFound`
- **Group 4 — PUBCOMP (Section 3.7)** (2 tests): correct packet ID + reason after full QoS 2 flow, message delivered after exchange
- **Group 5 — Outbound Server PUBREL** (1 test): server PUBREL has correct flags `0x02` and matching packet ID

One broker conformance gap discovered and fixed:
1. `handle_pubrel` sent PUBCOMP with `ReasonCode::Success` even when packet_id was not found in inflight — fixed to use `PacketIdentifierNotFound` (0x92) in both native and WASM brokers.

Added `RawMqttClient` methods: `expect_pubrec`, `expect_pubrel_raw`, `expect_pubcomp`, `expect_publish_qos2`.
Added `RawPacketBuilder` methods: `publish_qos2`, `pubrec`, `pubrel`, `pubrel_invalid_flags`, `pubcomp`.
Refactored `expect_puback` to use shared `parse_ack_packet` helper.

9 normative statements tracked in `conformance.toml` Sections 3.4–3.7: all Tested.

### 2026-02-17 — Section 3.2 CONNACK complete

11 passing tests across 5 groups:

- **Group 1 — CONNACK Structure** (2 raw-client tests): reserved flags zero, only one CONNACK per connection
- **Group 2 — Session Present + Error Handling** (3 raw-client tests): session present zero on error, error code closes connection, valid reason codes
- **Group 3 — CONNACK Properties** (3 tests): server capabilities present, MaximumQoS advertised when limited, assigned client ID uniqueness
- **Group 4 — Will Rejection** (2 raw-client + custom config tests): Will QoS exceeds maximum rejected with 0x9B, Will Retain rejected with 0x9A
- **Group 5 — Subscribe with Limited QoS** (1 high-level client test): subscribe accepted and downgraded when MaximumQoS < requested

Two broker conformance gaps discovered and fixed:
1. Will QoS exceeding `maximum_qos` was not rejected at CONNECT time — added validation in `connect.rs` for both native and WASM brokers `[MQTT-3.2.2-12]`
2. Will Retain=1 when `retain_available=false` was not rejected at CONNECT time — added validation in `connect.rs` for both native and WASM brokers `[MQTT-3.2.2-13]`

Additional fix: WASM broker was hardcoding `retain_available=true` in CONNACK instead of using the config value.

Added `RawMqttClient` method: `expect_connack_packet` (returns fully decoded `ConnAckPacket`).
Added `RawPacketBuilder` methods: `connect_with_will_qos`, `connect_with_will_retain`, `subscribe`.

22 normative statements tracked in `conformance.toml` Section 3.2: 8 Tested, 3 CrossRef, 7 NotApplicable (client-side), 3 Untested (max packet size constraints, keep alive passthrough).

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
