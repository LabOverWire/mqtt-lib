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
- [x] Section 3.14 — DISCONNECT (8 tests, 4 normative statements)
- [x] Section 4.7 — Topic Names and Topic Filters (10 tests, 5 normative statements)
- [x] Section 4.8 — Subscriptions / Shared Subscriptions (10 tests, 4 normative statements)
- [x] Section 3.3 Advanced — Overlapping Subs, Message Expiry, Response Topic (7 tests, 10 normative statements)
- [x] Final 6 untested statements — 3 tests, 2 NotApplicable, 1 NotImplemented
- [x] Section 4.12 — Enhanced Authentication (7 tests, 8 normative statements)
- [x] Section 1.5 — Data Representation (5 tests, 4 normative statements)
- [x] Section 4.13 — Error Handling (2 tests, 2 normative statements)
- [x] Section 6 — WebSocket Transport (3 tests, 3 normative statements)
- [x] Section 4.9 — Flow Control (3 tests, 3 normative statements)
- [x] Section 3.15 — AUTH reserved flags (1 test, 1 normative statement)
- [x] Triage — 44 statements reclassified (16 CrossRef, 28 NotApplicable)
- [x] Phase 0 — Broker MaxPacketSize enforcement implementation
- [x] Phase 1 — Extended CONNECT/Session/Will (14 tests, 19 normative statements)
- [x] Phase 3 — QoS Protocol State Machine (12 tests, 15 normative statements)
- [x] Final 7 remaining untested — 7 tests, 0 remaining Untested

**Rule**: after every step, every detail learned, every fix applied — add an entry here. New entries go on top, beneath this plan list.

---

## Diary Entries

### 2026-02-19 — Final 7 untested conformance statements resolved (zero remaining)

Added `section3_final_conformance.rs` with 7 tests covering the last 7 Untested normative statements. All 247 statements now accounted for: 174 Tested, 27 CrossRef, 44 NotApplicable, 2 Skipped.

Infrastructure additions to `raw_client.rs`:
- `RawPacketBuilder::connect_with_invalid_utf8_will_topic()` — CONNECT with Will Flag=1 and `[0xFF, 0xFE]` in Will Topic
- `RawPacketBuilder::connect_with_invalid_utf8_username()` — CONNECT with Username Flag and `[0xFF, 0xFE]` in Username
- `RawPacketBuilder::publish_qos2_with_message_expiry()` — QoS 2 PUBLISH with Message Expiry Interval property (0x02)
- `RawMqttClient::expect_disconnect_raw()` — returns raw DISCONNECT bytes for property inspection

Statements tested:
- MQTT-3.1.2-29: Request Problem Information=0 suppresses Reason String and User Properties on SUBACK
- MQTT-3.1.3-11: Will Topic with invalid UTF-8 causes connection close
- MQTT-3.1.3-12: Username with invalid UTF-8 causes connection close
- MQTT-3.14.0-1: Server does not send DISCONNECT before CONNACK (verified with invalid protocol version)
- MQTT-3.14.2-2: Server DISCONNECT contains no Session Expiry Interval property (verified via raw byte scan)
- MQTT-4.3.3-7: Broker sends PUBREL even after message expiry elapsed once PUBLISH was sent to subscriber
- MQTT-4.3.3-13: Broker sends PUBCOMP even after message expiry elapsed, continuing QoS 2 sequence

Final test suite: 205 tests across 21 test files, all passing. Clippy clean across entire workspace.

### 2026-02-19 — Phase 5: Shared Subscriptions + Flow Control conformance tests

Added 2 tests to `section4_shared_sub.rs` and created `section4_flow_control.rs` with 3 tests, covering 8 normative statements total.

Shared Subscription tests added:
- `shared_sub_respects_granted_qos` [MQTT-4.8.2-3]: subscribe at QoS 0, publish at QoS 1, verify delivered at QoS 0
- `shared_sub_puback_error_discards` [MQTT-4.8.2-6]: two shared subscribers, one sends PUBACK with 0x80, verify no redistribution to other subscriber

Flow Control tests created:
- `flow_control_quota_enforced` [MQTT-4.9.0-1, MQTT-4.9.0-2]: connect with receive_maximum=2, publish 5 QoS 1 messages, verify only 2 arrive before PUBACKs, then verify more arrive after PUBACKing
- `flow_control_other_packets_at_zero_quota` [MQTT-4.9.0-3]: connect with receive_maximum=1, fill quota, verify PINGREQ and SUBSCRIBE still work at zero quota
- `auth_invalid_flags_malformed` [MQTT-3.15.1-1]: send AUTH packet with non-zero reserved flags (0xF1), verify disconnect

Two helper functions added to `section4_flow_control.rs`:
- `read_all_available()`: accumulates raw bytes from TCP reads until timeout
- `extract_publish_ids()`: walks MQTT frame structure in raw bytes, counts PUBLISH packets and extracts packet IDs

One broker conformance gap discovered and fixed:
1. `handle_puback` and `handle_pubcomp` in `publish.rs` removed entries from `outbound_inflight` but never drained queued messages from storage. When a client's receive_maximum was hit, `send_publish` correctly queued messages to storage, but nothing pulled them back out (except `deliver_queued_messages` during session reconnect). Added `drain_queued_messages()` method to `ClientHandler` called from both `handle_puback` and `handle_pubcomp`.

Statements skipped:
- MQTT-4.8.2-4 (implementation-specific delivery strategy) — too complex, marked Skipped
- MQTT-4.8.2-5 (implementation-specific delivery on disconnect) — too complex, marked Skipped

8 conformance.toml updates: MQTT-4.8.2-3 Tested, MQTT-4.8.2-4 Skipped, MQTT-4.8.2-5 Skipped, MQTT-4.8.2-6 Tested, MQTT-4.9.0-1 Tested, MQTT-4.9.0-2 Tested, MQTT-4.9.0-3 Tested, MQTT-3.15.1-1 Tested.

### 2026-02-19 — Phase 7: WebSocket Transport conformance tests

Added `section6_websocket.rs` with 3 tests covering Section 6 WebSocket transport:

- `websocket_text_frame_closes` — [MQTT-6.0.0-1]: verifies server closes connection on text frame
- `websocket_packet_across_frames` — [MQTT-6.0.0-2]: verifies server reassembles MQTT packets split across WebSocket frames
- `websocket_subprotocol_is_mqtt` — [MQTT-6.0.0-4]: verifies server returns "mqtt" subprotocol in handshake

Infrastructure changes:
- Added `ws_local_addr()` to `MqttBroker` for retrieving WebSocket listener address
- Added `start_with_websocket()`, `ws_port()`, `ws_socket_addr()` to `ConformanceBroker`
- Fixed [MQTT-6.0.0-1] compliance: `WebSocketStreamWrapper::poll_read` now returns an error on text frames instead of silently ignoring them
- Added `tokio-tungstenite`, `futures-util`, `http` dev-dependencies to conformance crate
- Fixed pre-existing compilation error: added missing `extract_auth_method_property` function (was already added by linter)

### 2026-02-19 — Phase 4: Data Representation + Error Handling conformance tests

Added `section1_data_repr.rs` with 5 tests covering Section 1.5 data representation rules, and `section4_error_handling.rs` with 2 tests covering Section 4.13 error handling.

Infrastructure additions to `raw_client.rs`:
- `RawPacketBuilder::connect_with_surrogate_utf8()` — CONNECT with UTF-16 surrogate codepoint (U+D800) in client_id
- `RawPacketBuilder::connect_with_non_minimal_varint()` — CONNECT with 5-byte variable byte integer (exceeds 4-byte max)
- `RawPacketBuilder::publish_with_invalid_utf8_user_property()` — PUBLISH with invalid UTF-8 bytes in user property key
- `RawPacketBuilder::publish_with_oversized_topic()` — PUBLISH with 65535-byte topic (max UTF-8 string length)
- `RawPacketBuilder::subscribe_raw_topic()` — SUBSCRIBE with raw bytes as topic filter (for BOM testing)
- `RawPacketBuilder::publish_qos0_raw_topic()` — QoS 0 PUBLISH with raw bytes as topic name

Statements tested:
- MQTT-1.5.4-1: UTF-16 surrogate codepoints in UTF-8 strings must be rejected
- MQTT-1.5.4-3: BOM (U+FEFF) is valid in UTF-8 strings and must not be stripped
- MQTT-1.5.5-1: variable byte integers exceeding 4 bytes must be rejected
- MQTT-1.5.7-1: invalid UTF-8 in user property key/value must be rejected
- MQTT-4.7.3-3: max-length topic (65535 bytes) must be handled without crash
- MQTT-4.13.1-1: malformed packets (invalid packet type 0) must close connection
- MQTT-4.13.2-1: protocol errors (second CONNECT) must trigger DISCONNECT with reason >= 0x80

Incidental fix: borrow conflict in `drain_queued_messages()` in `publish.rs` — changed `ref client_id` pattern to `.clone()` to avoid simultaneous immutable/mutable borrow of `self`.

### 2026-02-19 — Phase 6: Enhanced Authentication conformance tests

Added `section4_enhanced_auth.rs` with 7 tests covering 8 normative statements from Section 4.12.

Infrastructure additions:
- `ConformanceBroker::start_with_auth_provider()` in harness.rs — starts broker with custom `AuthProvider`
- `RawPacketBuilder::connect_with_auth_method()` — CONNECT with Authentication Method property (0x15)
- `RawPacketBuilder::connect_with_auth_method_and_data()` — CONNECT with Method + Data properties
- `RawPacketBuilder::auth_with_method()` — AUTH packet with reason code and method property
- `RawPacketBuilder::auth_with_method_and_data()` — AUTH packet with reason code, method, and data
- `RawMqttClient::expect_auth_packet()` — parse AUTH response extracting reason code and method
- `extract_auth_method_property()` — helper to pull Authentication Method from raw property bytes

Test auth provider: `ChallengeResponseAuth` implements `AuthProvider` with `supports_enhanced_auth() -> true`. First call (no auth_data) returns Continue with challenge bytes. Second call checks response against expected value. Supports re-authentication via same logic.

Statements tested:
- MQTT-4.12.0-1: unsupported auth method → CONNACK 0x8C (AllowAllAuth broker)
- MQTT-4.12.0-2: server sends AUTH with reason 0x18 during challenge-response
- MQTT-4.12.0-3: client AUTH continue must use reason 0x18 (covered by same test as -2)
- MQTT-4.12.0-4: auth failure → connection closed (wrong response data)
- MQTT-4.12.0-5: auth method consistent across all AUTH packets in flow
- MQTT-4.12.0-6: plain CONNECT (no auth method) → no AUTH packet from server
- MQTT-4.12.0-7: unsolicited AUTH after plain CONNECT → disconnect
- MQTT-4.12.1-2: re-auth failure → DISCONNECT and close

### 2026-02-19 — Expand conformance.toml with 123 missing MQTT- IDs

Cross-referenced `conformance.toml` (124 IDs) against rfc-extract's `mqtt-v5.0-compliance.toml` (229 IDs). Added 123 previously untracked normative statements, bringing total to 247 unique IDs.

New sections created (14): 1.5 Data Representation (4), 2.1 Structure of an MQTT Control Packet (1), 2.2 Variable Header (6), 3.15 AUTH (4), 4.1 Session State (2), 4.2 Network Connections (1), 4.3 Quality of Service Levels (18), 4.4 Message Delivery Retry (2), 4.5 Message Receipt (2), 4.6 Message Ordering (1), 4.9 Flow Control (3), 4.12 Enhanced Authentication (9), 4.13 Handling Errors (2), 6.0 WebSocket Transport (4).

Existing sections expanded: 3.1 (+27), 3.4 (+2), 3.5 (+2), 3.6 (+2), 3.7 (+2), 3.8 (+9), 3.9 (+1), 3.10 (+6), 3.11 (+2), 3.14 (+4), 4.7 (+3), 4.8 (+4).

9 duplicate IDs in the extractor (same ID at two normative levels) resolved by picking the primary obligation (Must over May, MustNot over May). All 123 entries added as `status = "Untested"` with empty `test_names`. Updated `total_statements` for all affected sections. Added `title` and `total_statements` to sections 3.1, 3.2, 3.3 which previously lacked them.

### 2026-02-18 — MQTT-3.3.4-8 inbound receive maximum enforcement

Implemented server-side receive maximum enforcement:
- Added `server_receive_maximum: Option<u16>` to `BrokerConfig` with builder method
- `ClientHandler` stores resolved value (default 65535)
- CONNACK advertises receive maximum when configured
- `handle_publish` checks `inflight_publishes.len() >= server_receive_maximum` before processing QoS 1/2
- Sends DISCONNECT 0x93 (`ReceiveMaximumExceeded`) when exceeded
- Key insight: QoS 1 PUBACK is sent synchronously so QoS 1 inflight is transient; QoS 2 accumulates in `inflight_publishes` until PUBREL/PUBCOMP
- Conformance test sends 3 QoS 2 PUBLISHes with receive_maximum=2, asserts DISCONNECT 0x93 on 3rd

### 2026-02-18 — Final 6 untested conformance statements resolved

3 new tests across 3 files, plus 3 reclassifications:

- **`client_id_rejected_with_0x85`** in `section3_connect.rs`: raw CONNECT with `bad/id` client ID rejected with 0x85 `[MQTT-3.1.3-5]`
- **`server_keep_alive_override`** in `section3_connack.rs`: broker with `server_keep_alive=30s` returns `ServerKeepAlive=30` in CONNACK `[MQTT-3.2.2-22]`
- **`receive_maximum_limits_outbound_publishes`** in new `section3_publish_flow.rs`: raw subscriber with `receive_maximum=2` receives exactly 2 QoS 1 publishes when 4 are sent without PUBACKs `[MQTT-3.3.4-7]`

Added `RawPacketBuilder::connect_with_receive_maximum()` to build CONNECT with Receive Maximum property.

Key implementation detail: `read_packet_bytes()` can return multiple MQTT packets in a single TCP read, so the receive maximum test accumulates all bytes and counts PUBLISH packet headers by walking the MQTT frame structure.

6 conformance.toml updates: 3 Untested→Tested (`MQTT-3.1.3-5`, `MQTT-3.2.2-22`, `MQTT-3.3.4-7`), 2 Untested→NotApplicable (`MQTT-3.2.2-19`, `MQTT-3.2.2-20` — broker never reads client Maximum Packet Size), 1 Untested→NotImplemented (`MQTT-3.3.4-8` — broker does not enforce inbound receive maximum).

### 2026-02-18 — Section 3.3 Overlapping Subscriptions, Message Expiry & Response Topic complete

7 passing tests across 3 groups in `section3_publish_advanced.rs`:

- **Group 1 — Overlapping Subscriptions** (3 tests): two wildcard filters deliver 2 copies with max QoS respected `[MQTT-3.3.4-2]`, each copy carries its matching subscription identifier `[MQTT-3.3.4-3]`/`[MQTT-3.3.4-5]`, no_local prevents echo on wildcard overlap
- **Group 2 — Message Expiry** (2 tests): expired retained message not delivered to new subscriber `[MQTT-3.3.2-5]`, retained message expiry interval decremented by server wait time `[MQTT-3.3.2-6]`
- **Group 3 — Response Topic** (2 tests): wildcard in Response Topic causes disconnect `[MQTT-3.3.2-14]`, valid UTF-8 Response Topic forwarded to subscriber `[MQTT-3.3.2-13]`

One broker conformance gap discovered and fixed:
1. `handle_publish()` in `publish.rs` never validated the Response Topic property for wildcards — added `validate_topic_name()` call on the Response Topic after the main topic validation `[MQTT-3.3.2-14]`.

Added `RawPacketBuilder` methods: `publish_qos0_with_response_topic`, `subscribe_with_sub_id`.

10 normative statements updated in `conformance.toml`: 7 from Untested to Tested (`MQTT-3.3.2-5`, `MQTT-3.3.2-6`, `MQTT-3.3.2-13`, `MQTT-3.3.2-14`, `MQTT-3.3.4-2`, `MQTT-3.3.4-3`, `MQTT-3.3.4-5`), 2 to NotApplicable (`MQTT-3.3.4-4`, `MQTT-3.3.4-10`), 1 to CrossRef (`MQTT-3.3.2-19`).

### 2026-02-17 — Section 3.3 Topic Alias Lifecycle & DUP Flag tests complete

6 passing tests across 2 groups in `section3_publish_alias.rs`:

- **Group 1 — Topic Alias Lifecycle** (5 tests): register alias and reuse via empty-topic PUBLISH `[MQTT-3.3.2-12]`, remap alias to different topic `[MQTT-3.3.2-12]`, alias not shared across connections `[MQTT-3.3.2-10]`/`[MQTT-3.3.2-11]`, alias cleared on reconnect `[MQTT-3.3.2-10]`/`[MQTT-3.3.2-11]`, alias stripped before delivery (subscriber receives full topic name)
- **Group 2 — DUP Flag** (1 test): DUP=1 on incoming QoS 1 PUBLISH is not propagated to subscriber `[MQTT-3.3.1-3]`

One broker conformance gap discovered and fixed:
1. `prepare_message()` in `router.rs` cloned the incoming PUBLISH but never cleared the `dup` flag — DUP=1 from the publisher would propagate to subscribers. Added `message.dup = false;` after the clone `[MQTT-3.3.1-3]`.

Added `RawMqttClient` methods: `expect_publish_raw_header`.
Added `RawPacketBuilder` methods: `publish_qos0_with_topic_alias`, `publish_qos0_alias_only`, `publish_qos1_with_dup`.

4 normative statements updated in `conformance.toml` from Untested to Tested: `MQTT-3.3.1-3`, `MQTT-3.3.2-10`, `MQTT-3.3.2-11`, `MQTT-3.3.2-12`.

### 2026-02-17 — Section 4.8 Shared Subscriptions complete

8 passing tests across 4 groups in `section4_shared_sub.rs`:

- **Group 1 — Shared Subscription Format Validation** (3 tests): valid `$share/mygroup/sensor/+` accepted `[MQTT-4.8.2-1]`, ShareName with `+` or `#` returns 0x8F `[MQTT-4.8.2-2]`, incomplete `$share/grouponly` (no second `/`) returns 0x8F `[MQTT-4.8.2-1]`
- **Group 2 — Message Distribution** (2 tests): two shared subscribers get ~3 messages each from 6 published (round-robin), mixed shared+regular both receive all messages when shared group has single member
- **Group 3 — Retained Messages** (1 test): shared subscription does not receive retained messages on subscribe, regular subscription does
- **Group 4 — Unsubscribe and Multiple Groups** (2 tests): unsubscribe from shared stops delivery, two independent groups (`groupA`, `groupB`) each receive a copy of published messages

Two broker conformance gaps discovered and fixed:
1. `handle_subscribe` in both native and WASM brokers never validated ShareName characters — `$share/gr+oup/topic` and `$share/gr#oup/topic` were silently accepted. Added `parse_shared_subscription()` and check for `+` or `#` in group name, returning `TopicFilterInvalid` (0x8F) `[MQTT-4.8.2-2]`.
2. Incomplete shared subscription format `$share/grouponly` (no second `/` after ShareName) was silently accepted as a regular subscription. Added check: if filter starts with `$share/` but `parse_shared_subscription()` returns no group, reject with `TopicFilterInvalid` (0x8F) `[MQTT-4.8.2-1]`.

Removed now-unused `strip_shared_subscription_prefix` import from both broker subscribe handlers (replaced by direct `parse_shared_subscription` call).

2 normative statements tracked in `conformance.toml` Section 4.8: all Tested.

### 2026-02-17 — Section 4.7 Topic Names and Topic Filters complete

10 passing tests across 4 groups in `section4_topic.rs`:

- **Group 1 — Topic Filter Wildcard Rules** (4 tests): `#` not last in filter returns 0x8F `[MQTT-4.7.1-1]`, `tennis#` (not full level) returns 0x8F `[MQTT-4.7.1-1]`, `sport+` and `sport/+tennis` both return 0x8F `[MQTT-4.7.1-2]`, valid wildcards (`sport/+`, `sport/#`, `+/tennis/#`, `#`, `+`) all granted QoS 0
- **Group 2 — Dollar-Prefix Topic Matching** (2 tests): `#` does not match `$SYS/test` and `+/info` does not match `$SYS/info` `[MQTT-4.7.2-1]`, explicit `$SYS/#` subscription matches `$SYS/test`
- **Group 3 — Topic Name/Filter Minimum Rules** (2 tests): empty string filter returns 0x8F `[MQTT-4.7.3-1]`, null char in topic name causes disconnect `[MQTT-4.7.3-2]`
- **Group 4 — Topic Matching Correctness** (2 tests): `sport/+/player` matches one level only, `sport/#` matches `sport`, `sport/tennis`, and `sport/tennis/player`

One broker conformance gap discovered and fixed:
1. `handle_subscribe` in both native and WASM brokers never validated topic filters — malformed filters like `sport/tennis#` or `sport+` were silently accepted. Added `validate_topic_filter()` call (with `strip_shared_subscription_prefix()` for shared subscriptions) returning `TopicFilterInvalid` (0x8F) per-filter in the SUBACK.

5 normative statements tracked in `conformance.toml` Section 4.7: all Tested.

### 2026-02-17 — Section 3.14 DISCONNECT complete

8 passing tests across 4 groups in `section3_disconnect.rs`:

- **Group 1 — Will Suppression/Publication** (3 tests): normal disconnect (0x00) suppresses will `[MQTT-3.14.4-3]`, disconnect with 0x04 (`DisconnectWithWillMessage`) publishes will, TCP drop publishes will after keep-alive timeout
- **Group 2 — Reason Code Handling** (2 tests): valid reason codes (0x00, 0x04, 0x80) accepted `[MQTT-3.14.2-1]`, invalid reason code (0x03) rejected
- **Group 3 — Server-Initiated Disconnect** (2 tests): second CONNECT triggers server DISCONNECT, server DISCONNECT uses valid reason code
- **Group 4 — Post-Disconnect Behavior** (1 test): no PINGRESP after client DISCONNECT `[MQTT-3.14.4-1]`/`[MQTT-3.14.4-2]`

One broker conformance gap discovered and fixed:
1. `handle_disconnect` in both native and WASM brokers unconditionally set `normal_disconnect = true` and cleared the will message for ALL DISCONNECT reason codes — including 0x04 (`DisconnectWithWillMessage`). Fixed to only suppress will when reason code is NOT 0x04.

Added `RawMqttClient` methods: `expect_disconnect_packet`.
Added `RawPacketBuilder` methods: `disconnect_normal`, `disconnect_with_reason`, `connect_with_will_and_keepalive`.

4 normative statements tracked in `conformance.toml` Section 3.14: all Tested. Session Expiry override rules deferred (complex, not critical path).

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
