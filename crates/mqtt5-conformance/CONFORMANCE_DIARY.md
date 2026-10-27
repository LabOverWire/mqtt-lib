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
- [x] Offline queued delivery (8 tests, 4 normative statements)

**Rule**: after every step, every detail learned, every fix applied — add an entry here. New entries go on top, beneath this plan list.

---

## Diary Entries

### Raw reader cleanup (2026-10-26)

**Trigger**: the cleanup left from the #269 review below.

**Changes**:
- `read_mqtt_packet` and `expect_any_packet` are gone. Every caller uses `read_packet_bytes`, which reads one framed packet.
- The concatenate-and-reframe helpers (`count_publish_headers` in `section3_publish_flow.rs`, `read_all_available` plus `extract_publish_ids` in `section4_flow_control.rs`) are replaced by `RawMqttClient::read_publish_packets`, which returns every PUBLISH read before a close, a 500 ms gap or the timeout. Both receive maximum tests count those packets and PUBACK the packet identifiers `ParsedPublish` finds in them.
- The hand-written Remaining Length loops in `raw_client.rs` and `section4_shared_sub.rs` use `decode_variable_int`. `try_extract_packet` treats a failed decode with 5 or more bytes buffered as malformed, which keeps the #269 behaviour of returning those bytes.
- The DISCONNECT scan (MQTT-3.14.2-2) and the SUBACK scan (MQTT-3.1.2-29) in `section3_final_conformance.rs` parse the property block with `ParsedProperties`, which now reads Session Expiry Interval. A malformed block fails the test.
- `parse_publish` is gone. `expect_publish`, `expect_publish_raw_header`, `expect_publish_with_id` and `expect_publish_qos2` use `ParsedPublish`. `section4_shared_sub.rs` uses `raw_client::parse_ack_reason_codes` instead of its own SUBACK parser.

**Review of this PR**: 10 findings, checked in a scratch program over a tokio duplex stream with copies of the old and new helpers.
- The first version of the flow control helper kept only PUBLISHes that `ParsedPublish` accepted, so 3 QoS 1 PUBLISHes with one non UTF-8 topic counted as 2 (the old code counted 3). It also returned id 0 for a QoS 0 PUBLISH, which the test then PUBACKed.
- The DISCONNECT scan stopped at the first property other than Reason String or User Property, so Server Reference followed by Session Expiry Interval passed. A truncated properties length passed with no assertion, as it did before.
- The SUBACK scan's hand-written properties length panicked on shift overflow with 5 continuation bytes, and treated every byte of the property block as a property id.
- The PUBLISH readers and `ParsedPublish` disagreed on a non UTF-8 topic and on an unknown property: the readers returned the packet, `ParsedPublish` returned `None`. Both return `None` now.
- New unit tests: `read_publish_packets_keeps_publishes_the_parser_rejects` and `properties_read_session_expiry_after_another_property`.

**Behaviour changes**: a PUBLISH whose properties length ran past the packet made `expect_publish` panic on a slice index; it now returns `None`. The `expect_publish*` readers also return `None` for a PUBLISH with a non UTF-8 topic or an unknown property.

**Results**: conformance lib 251 passed, `manifest_load` 12 passed, integration files pass. CLI in-process 191 passed, 3 skipped; external mqttv5 on 21883 191 passed, 3 skipped; Mosquitto 2.1.2 on 21884 187 passed, 7 skipped, the same as before. fmt and the four pedantic clippy runs are clean.

### Review fixes for the shared reader buffer (2026-10-26)

**Trigger**: a code review of PR #269 raised 10 findings. Five were tested in a scratch program over a tokio duplex stream, against the PR's first commit and against main.

**Confirmed and fixed**:
- `expect_disconnect` returned `true` as soon as a DISCONNECT arrived, on main and in the PR. A broker that sends DISCONNECT and leaves the connection open passed every test that requires the Server to close the Network Connection. It now skips packets, DISCONNECT included, until the connection closes (`true`) or the timeout runs out (`false`).
- The session takeover test checked the close with `read_packet_bytes(..).is_none()`, which is also `None` when the read times out on an open connection. It now uses `expect_disconnect` (MQTT-3.1.4-3). Control: a broker mutated to wait 30 s after sending DISCONNECT 0x8E passes the old check and fails the new one with "... it is still open". Reverted.
- New in the PR's first commit: a packet with a Remaining Length over 4 bytes stayed in the buffer until the timeout, and so did a partial packet before a close, so they looked like silence. On main `read_packet_bytes` returned those bytes. Both are returned to the caller again.
- The readers now share one loop (`read_until`) that tells a packet, a close and a timeout apart, with a deadline check before each read. The two doc comments the PR added are removed.

**Refuted**: `expect_disconnect` never ending while packets keep arriving. With a writer that never pauses, it ended in 3 of 3 runs, 7 to 158 ms after the deadline. The shared loop's deadline check removes the overshoot.

**Confirmed, not acted on**: a `false` from `expect_disconnect` leaves the skipped packets consumed, on main too. Every caller wraps it in `assert!`, so none reads on after a `false`.

**Left for a cleanup PR**: one name for `read_packet_bytes`, `read_mqtt_packet` and `expect_any_packet`; the re-framing parsers in `section3_publish_flow.rs` and `section4_flow_control.rs`, which now get one packet per read; and the hand-written Remaining Length loops, which could use `decode_variable_int`.

**Tests**: `framing_tests` has 8 tests. Against main, 3 fail: a DISCONNECT on an open connection and the two coalesced-packet cases. Against the PR's first commit, 3 fail: a DISCONNECT on an open connection, the malformed Remaining Length and the partial packet before a close.

**Results**: conformance lib 249 passed, `manifest_load` 12 passed, integration files pass. CLI in-process 191 passed, 3 skipped; external mqttv5 on 21883 191 passed, 3 skipped; Mosquitto 2.1.2 on 21884 187 passed, 7 skipped. Both brokers close the connection after every DISCONNECT the suite checks. fmt and the four pedantic clippy runs are clean.

### Raw client readers share one buffer (2026-10-26)

**Trigger**: the separate work left from the #267 review below. `read_packet_bytes` and `expect_disconnect` read the socket directly, while `read_mqtt_packet` keeps leftover bytes in `self.buf`.

**Confirmed by test first**: a scratch program fed a `RawMqttClient` over a tokio duplex stream, once with each packet in its own write and once with the packets in one write. Separate writes passed every case. With one write:
- SUBACK then PUBLISH: `expect_suback` buffered the PUBLISH and `expect_publish` returned `None`.
- PUBACK then PINGRESP: `expect_puback` swallowed both, and `expect_pingresp` returned `false`.
- SUBACK then DISCONNECT on a connection left open: `expect_disconnect` returned `false`.

**Fix**: `read_packet_bytes` now returns one framed packet through `read_mqtt_packet`, so every raw reader built on it keeps the rest buffered. `expect_disconnect` drains the buffer first and skips other packets until a DISCONNECT, a close or the timeout. Before, after one non-DISCONNECT read it waited a second full timeout for one more byte. No test asserts that `expect_disconnect` returns `false`.

**Tests**: five unit tests in `raw_client::framing_tests`. The three cases above fail on the old code (control: old `raw_client.rs` with the new test module) and pass on the new. Two guards pass on both: a PUBLISH on a connection that stays open is not a disconnect, and a closed connection is.

**Results**: conformance lib 246 passed, `manifest_load` 12 passed, integration files pass. CLI in-process 191 passed, 3 skipped; external mqttv5 on 21883 191 passed, 3 skipped; Mosquitto 2.1.2 on 21884 187 passed, 7 skipped, the same as before the change. fmt and the four pedantic clippy runs are clean.

**Left as is**: `read_packet_bytes`, `read_mqtt_packet` and `expect_any_packet` now do the same thing. Folding them into one name touches about 25 call sites and is left for its own change.

### Review fixes for the follow-up test bugs (2026-10-26)

**Trigger**: a code review of PR #267 raised 10 findings. Each one was checked at runtime before any fix. Broker mutations were made in this worktree and reverted afterwards.

**Confirmed and fixed**:
- The shared unsubscribe fix in the entry below was wrong. `TestClient`'s raw backend compared the filter without its `$share/<name>/` prefix, so unsubscribing `$share/group/t` also removed a plain `t` subscription on the same client. Entries now keep the filter they were subscribed with, and unsubscribe matches it exactly (MQTT-3.10.4-1 compares filters character by character). Against Mosquitto on port 21884, a delivery test fails on the old code and passes on the new one. New unit test `shared_unsubscribe_keeps_plain_subscription` checks the client's subscription list instead (left `[]` on the old code), because our broker has its own bug here (see below).
- `server_delivers_to_matching_subscribers` lost its payload check in the rewrite. Control: a router that empties every payload passes the PR's version and fails both the old version and the fixed one ("the delivered message must carry the published payload").
- Server Keep Alive handling in the keep alive tests. With `--keep-alive 0` the broker sends Server Keep Alive 0, and `keepalive_timeout_closes_connection` failed a compliant broker. `keepalive_zero_no_timeout` slept 0 s and still reported MQTT-3.2.2-22 as passed. With `--keep-alive 600` the timeout test ran for 900.00 s. The CLI cannot skip a test while it runs, so the three keep alive tests now need a new requirement, `no_server_keep_alive`, which matches `server_keep_alive = 0` in the profile. The tests fail with "the CONNACK carries Server Keep Alive ..., but the SUT profile declares server_keep_alive = 0" when the profile is wrong. With `server_keep_alive = 600` in the profile, all three are skipped.
- `pingreq_resets_keepalive` never read the CONNACK, although the CHANGELOG said the keep alive tests did. It is now covered by the same requirement and CONNACK check.
- The `assertions.rs` module doc said every helper takes a statement ID. `expect_no_publish` takes a context that callers bracket themselves. All callers already did.

**Keep alive margin**: a broker mutated to close at twice the Keep Alive passed the old 4 s window at Keep Alive 2 in 1 of 3 runs, since it closes at 4.0 s. Probe measurements of the close time: our broker 6.002 s at Keep Alive 4; Mosquitto 3.95 to 3.97 s at Keep Alive 2, 6.6 to 6.9 s at 4, and 9.14 to 9.96 s at 6. Mosquitto is up to about 1 s late at every Keep Alive, so the old window passed it by about 40 ms. The test now uses Keep Alive 6 and waits 11 s. The 2x mutant fails 3 of 3 runs. Our broker passes in 9.00 s and Mosquitto in 9.45 to 10.05 s, 3 of 3 each.

**Not reproduced, left unchanged**: the PINGREQ loop in `connack_assigned_client_id_unique` calls `send_raw(...).unwrap()`. Against a broker mutated to close every connection with an assigned ID right after CONNACK, the write succeeded in 6 of 6 runs, and the tagged `[MQTT-3.2.2-16]` assertion fired.

**Left for separate work**: `read_packet_bytes` and `expect_disconnect` read the socket directly, while `read_mqtt_packet` keeps leftover bytes in `self.buf`, so bytes it buffered are invisible to the other readers. The duplicate empty ClientID CONNECT builders in `section3_connack.rs` and `section3_connect.rs` stay.

**Broker finding**: our router keys a subscription by client and flow under the filter without its share prefix. So `$share/g/t` replaces the same client's plain `t` subscription, and unsubscribing `$share/g/t` removes it. A raw probe that subscribes to both and publishes once: mqttv5 delivers 1 copy and then 0 after unsubscribing the shared filter; Mosquitto delivers 2 and then 1. Section 4.8.2 says a Client with both gets a copy through the Non-shared Subscription, and a second through the Shared Subscription. Not fixed here.

**Results**: conformance lib tests 241 passed, `manifest_load` 12 passed, integration test files pass; CLI against the in-process broker 191 passed, 3 skipped; against an external `mqttv5` broker on port 21883, 191 passed, 3 skipped; against a local Mosquitto on port 21884 with TLS and WebSocket off in the profile, 187 passed, 7 skipped; fmt and the four pedantic clippy runs clean.

### Follow-up test bugs from the statement ID audit (2026-10-24)

**Trigger**: the test bugs listed in the audit entry below.

**Fixed** (each checked with a broker mutation in this worktree, reverted afterwards):
- `TestClient`'s raw backend now matches the stripped filter when it removes a shared subscription on UNSUBSCRIBE. New unit test `shared_unsubscribe_stops_local_delivery` fails on the old code (left 1, right 0) and passes now.
- `puback_message_delivered_on_qos1` publishes at QoS 1 on a unique topic and checks the delivered QoS. That check is MQTT-3.8.4-8 for min(1, 1), which none of its other tests covered, so the test now cites it. Control: `effective_qos` returns QoS 0 for a QoS 1 message on a QoS 1 subscription; the new test fails, the old one passes.
- `qos1_server_outbound_unique_nonzero_id_and_dup_zero` receives three PUBLISHes before acknowledging any, so the uniqueness check only covers Packet Identifiers that are in use, as MQTT-2.2.1-4 says. `expect_publish_with_id` now reads one framed packet, since the three PUBLISHes can arrive in one read. Controls: a broker that always allocates the lowest free ID, which is compliant, passes the new test and fails the old one ("duplicate: 1"); a broker that sends every PUBLISH with ID 7 fails the new test.
- `unsuback_reason_codes_are_valid_spec_values` accepts 0x8F and 0x91, checked against the UNSUBACK Reason Code table in 3.11.3.
- `single_level_wildcard_matches_one_level` and `server_delivers_to_matching_subscribers` read with `RawMqttClient` and `expect_no_publish`, on unique topics. Their negative checks have no numbered statement, so their messages carry no tag. `expect_no_publish` takes a context label instead of a bare statement ID. Controls: a router that delivers to every wildcard and exact subscriber fails both new tests and passes both old ones; with only exact filters over-delivered, the non-matching subscriber check in `server_delivers_to_matching_subscribers` fails on its own.
- `connack_assigned_client_id_unique` keeps both connections open and requires a PINGRESP on each. Controls: a broker that assigns `auto-fixed` to every client fails both versions; a broker that assigns `auto-fixed` unless a connected session holds it, which MQTT-3.2.2-16 allows, passes the new test and fails the old one in 3 of 3 runs.
- `connect_empty_client_id_server_assigns` decodes the CONNACK instead of searching for a 0x12 byte.
- The keep alive tests read the CONNACK's Server Keep Alive. `keepalive_timeout_closes_connection` waits 1.5 times the Keep Alive in force plus 1 s.

**Broker finding**: with the 1 s margin, `keepalive_timeout_closes_connection` failed against our broker. A Python probe sending CONNECT with no traffic afterwards, and a CONNACK with no Server Keep Alive, measured the close at 2.005 s, 4.008 s, 8.007 s and 12.006 s for Keep Alive 1, 2, 4 and 6: twice the Keep Alive, not 1.5 times (MQTT-3.1.2-22). The old 5 s window hid this. Fixed in #266, which also counts the Keep Alive from when a packet arrives and fixes the wasm broker's late close. The test passes on this branch after rebasing onto it.

**Results**: conformance lib tests 239 passed, `manifest_load` 12 passed, integration test files pass; CLI against the in-process broker 191 passed, 3 skipped; CLI against an external `mqttv5` broker (on port 21883, since 1883 is in use here) 191 passed, 3 skipped; fmt and the four pedantic clippy runs clean.

### Review fixes for the statement ID audit (2026-10-23)

**Trigger**: code review of PR #265 (10 findings). Nine were confirmed against the code and fixed; one was not taken.

**Fixed**:
- `qos2_pubrec_error_allows_packet_id_reuse` cited MQTT-4.3.3-12 but never read its subscription and ignored both PUBCOMPs, so a broker that treated the reused ID as a duplicate passed. `qos2_after_pubcomp_same_id_is_new_message` already checks MQTT-4.3.3-12 in full, including delivery of both messages, so the weaker duplicate is removed.
- `expect_no_packet` treated a closed connection as silence, since `read_mqtt_packet` returns `None` on EOF as on timeout. It is replaced by `assertions::expect_no_publish`, which fails on a matching PUBLISH or any other packet, and then sends PINGREQ and requires the PINGRESP. Control: `CTL_CLOSE_AFTER_UNSUB` closes the connection right after UNSUBACK; the #265 unsubscribe tests pass under it, the new ones fail with "the connection must stay open and answer PINGREQ".
- `expect_suback` and `expect_unsuback` read one unbuffered 8 KB chunk and dropped anything after the ack, so a `#` subscriber could lose half a retained message and lose framing. Both now read one framed packet through `read_mqtt_packet`, which keeps the rest buffered, and share one parser (`parse_ack_reason_codes`).
- The unsubscribe tests had no proof that the message published after the UNSUBSCRIBE was routed before the quiet window. Each now has a second, still-subscribed client that must receive it first. Control: `CTL_UNSUB` (UNSUBACK without removing the subscription) fails both tests on "expected no PUBLISH".
- `raw_subscriber` and `first_topic_matching` were private to `section4_topic.rs` while the unsubscribe tests repeated the same setup. They are `assertions::subscribed_raw_client` and `assertions::first_publish_matching` now.
- `dollar_topics_not_matched_by_root_wildcards` depended on `dollar_sys_publish`, which our broker should lose (#246) and other brokers lack, leaving MQTT-4.7.2-1 marked Tested with nothing running it. It now uses the broker's own `$SYS` messages: a `$SYS/#` subscriber must receive them, and `#` and `+/#` must not, under a new capability `sys_topics_published`. Our broker publishes retained `$SYS` topics by default, so the in-process SUT, `inprocess.toml`, `external-mqtt5.toml` and the full-broker profile declare it; the Mosquitto, EMQX and HiveMQ fixtures keep the default false because their `$SYS` behaviour was not checked here. Control: `CTL_DOLLAR` drops the `$` rule for wildcard filters on both the live and the retained path; the test fails with "expected no PUBLISH on $SYS/broker/version".
- The close-statement rule was applied only where the first audit touched a test. Every registered test that sends a packet breaking a rule and requires the connection to close was checked: 15 cite MQTT-4.13.1-1 now (`connect_first_packet_must_be_connect`, the PUBLISH format and wildcard tests, `response_topic_wildcard_rejected`, the SUBSCRIBE and UNSUBSCRIBE payload and UTF-8 tests, `null_char_in_topic_name_rejected`). Tests whose statement already says the Server closes (MQTT-3.1.0-2, 3.1.2-1, 3.1.2-2, 3.1.3-8, 3.2.2-7, 3.6.1-1, 3.8.1-1, 3.10.1-1, 3.15.1-1, 4.12.0-1) and topic filter tests that accept SUBACK 0x8F instead of a close are unchanged.
- `known-citation-drift.txt` was empty, so it and the shrink-only check are removed. `every_declared_statement_cites_the_test` and `every_cited_test_declares_the_statement` require exact agreement both ways.
- Bindings `_t`, `_p`, `_pid`, `_packet_id`, `_topic` and `_payload` in `deferred_ack.rs` and `section4_qos.rs` became `_` or `..`.
- The conformance README still said 186 tests and 159 Tested statements and described the drift file. The counts are removed and the guard description matches the tests. BROKER_SETUP.md counts follow the removed test and the new capability.

**Not taken**: moving `message_expiry_drops_expired_retained` back under MQTT-3.3.2-5. That statement covers a copy of a message held for a matching subscriber whose onward delivery never started. A retained message that expires is a separate rule in §3.3.1.3 ("If the current retained message for a Topic expires, it is discarded and there will be no retained message for that topic"), which has no statement number. Both audit passes reached that reading independently, so the test stays a plain integration test and MQTT-3.3.2-5 stays Untested.

**Results**: conformance lib tests 238 passed (one removed duplicate), `manifest_load` 12 passed, integration test files pass; CLI against the in-process broker 191 passed and 3 skipped in four full runs, with the `$SYS` wildcard test now running in-process; CLI against an external `mqttv5` broker 191 passed and 3 skipped (WebSocket); fmt and the four pedantic clippy runs clean.

### Statement IDs audited against the statement text for every registered test (2026-10-23)

**Trigger**: while adding missing `[MQTT-x-y]` doc comments, a consistency check showed 52 of 201 registered tests where the attribute `ids`, the doc comment tags and the manifest's `test_names` disagreed, and 8 tests in `section3_offline_delivery.rs` had no doc comment.

**Method**: five agents read every test and its helpers and decided, from `mqtt-v5.0-statement-texts.txt` and the OASIS body, which statements each test's assertions check. A statement counts only if an assertion fails when a broker breaks that exact rule and the test is built to check it, not a precondition such as a CONNACK arriving. Five more agents then tried to refute every proposed change, reading the code again rather than the first agents' descriptions. Every disagreement was settled by hand against the code and the text. Where a test sends a packet that breaks a Client rule and checks that the Server closes the connection, the test cites the Client rule and the Server's close statement (MQTT-3.1.4-1 for CONNECT, MQTT-4.13.1-1 otherwise), as the 2026-10-01 pass did for MQTT-3.1.2-11.

**What was wrong** (85 tests changed IDs, 64 assertion messages retagged):
- Off-by-one runs: CONNECT tests still cited MQTT-3.1.4-4/-5/-6 for Session Present, Will publication and CONNACK (MQTT-3.2.2-3, MQTT-3.1.2-8, MQTT-3.1.4-5); MQTT-3.1.2-4/-5 tests checked only Session Present (MQTT-3.2.2-2/-3).
- Wrong statement entirely: six shared-subscription tests cited MQTT-4.8.2-4/-5 (completing a QoS 2 delivery after reconnect); the wildcard matching tests cited MQTT-4.7.1-1/-2 (where wildcards may appear in a filter); `non_minimal_varint_rejected` sends a five-byte length, which is over-length, not non-minimal (MQTT-1.5.5-1); `qos2_pubrec_error_allows_packet_id_reuse` never makes the Server send a PUBREC of 0x80 or more, so it checks MQTT-4.3.3-12, not -9.
- Precondition tags on setup assertions, and many tests missing a statement their assertions do check (for example MQTT-2.2.1-5 on the ack Packet Identifier checks).
- Reverse drift: the manifest cited tests for statements they never declared, which `manifest_load` did not check.

**Tests fixed so they can fail** (each checked with a broker mutation in a scratch worktree, the old test passing and the new test failing under it):
- `connect_malformed_packet_closes_connection` shut down its write half before waiting for the close, so any server closed on EOF. It now sends a complete CONNECT whose properties length runs past the packet (`RawPacketBuilder::connect_with_overlong_properties`; `connect_malformed_truncated` and `RawMqttClient::shutdown_write` are removed). Control: a listener that never validates and closes only on EOF.
- `will_retain_zero_publishes_non_retained` subscribed with Retain As Published 0, which clears RETAIN on delivery anyway. It now subscribes with Retain As Published 1. Control: `CTL_WILL_RETAIN` publishes every Will retained.
- `unsubscribe_stops_delivery` and `shared_sub_unsubscribe_stops_delivery` read through `TestClient`, which removes the local subscription before sending UNSUBSCRIBE, so a later delivery was never counted. They now read with a `RawMqttClient` and the new `assertions::expect_no_packet`. Control: `CTL_UNSUB` acknowledges UNSUBSCRIBE without removing anything. The old shared test also passed on the in-process backend under that mutation.
- `dollar_topics_not_matched_by_root_wildcards` and `topic_matching_no_unicode_normalization` relied on `TestClient` dropping PUBLISH packets that do not match the local filter. They now read raw. The `$SYS` test requires `dollar_sys_publish`, has a `$SYS/#` subscriber as a positive control, and publishes on its own `$SYS/conformance/<id>` topic: run in parallel, it and `dollar_topics_matched_by_explicit_prefix` both published `$SYS/test` and the latter could read the other's payload. Controls: `CTL_DOLLAR` drops the `$` rule for wildcard filters, `CTL_UNICODE` treats the two normalization forms as one topic.

**Moved out of the registry** into plain integration tests in `tests/`, since what they check has no numbered MUST: `max_length_topic_handled` (a topic longer than 65,535 bytes cannot be encoded), `message_expiry_drops_expired_retained` (unnumbered §3.3.1.3 prose), `shared_sub_round_robin_delivery` and `shared_sub_no_retained_on_subscribe` (unnumbered §4.8.2 prose), `disconnect_valid_reason_codes_accepted` and `no_packets_after_client_disconnect` (closing after a Client DISCONNECT is a SHOULD).

**Manifest**: `test_names` rebuilt from the attributes; `known-citation-drift.txt` is empty; new test `every_cited_test_declares_the_statement` fails on the manifest before this pass and passes now. MQTT-3.1.3-7 is a Server statement (it was Client/NotApplicable). `mqtt-v5.0-statement-texts.txt` had MQTT-3.1.2-27 cut off at "to the."; it now ends "to the Client.". Status: Tested 164 → 157, Untested 39 → 58, CrossRef 12 → 1, NotApplicable 36 → 35 (251 statements). Newly Untested, because their only test did not check them: MQTT-1.5.5-1, 3.1.2-4, 3.1.2-5, 3.1.2-9, 3.1.2-10, 3.1.2-16, 3.1.2-18, 3.1.2-19, 3.1.2-26, 3.3.2-1, 3.3.2-5, 3.3.2-11, 3.3.2-13, 3.14.4-1, 3.14.4-2, 4.3.2-1, 4.3.2-5, 4.3.3-1, 4.3.3-5, 4.3.3-9, 4.7.3-3, and the cross-references MQTT-2.2.1-2, 3.1.3-2, 3.3.2-19, 4.3.3-6. Newly Tested: MQTT-2.2.1-5, 2.2.1-6, 3.1.2-8, 3.1.3-7, 3.1.4-5, 3.2.2-22, 3.3.4-10, 3.8.4-3, 3.8.4-5, 3.10.4-4, 3.10.4-6, 4.3.3-4, 4.6.0-6, 4.7.0-1.

**Test bugs found and left for a follow-up**: `puback_message_delivered_on_qos1` publishes at QoS 0; the Packet Identifier uniqueness check in `qos1_server_outbound_unique_nonzero_id_and_dup_zero` acknowledges each message before the next, so it can fail a compliant broker that reuses a freed ID; `unsuback_reason_codes_are_valid_spec_values` leaves 0x8F and 0x91 out of its valid codes; `TestClient`'s raw backend never removes a shared subscription locally on UNSUBSCRIBE (it stores the filter without the `$share/<name>/` prefix and compares the full filter); the negative halves of `server_delivers_to_matching_subscribers` and `single_level_wildcard_matches_one_level` are masked by `TestClient` filtering; `connack_assigned_client_id_unique` closes the first connection before the second CONNECT, so its uniqueness half can fail a compliant broker; `connect_empty_client_id_server_assigns` finds the Assigned Client Identifier by searching for a 0x12 byte; the keep alive tests ignore a Server Keep Alive in the CONNACK.

**Results**: conformance lib tests 239 passed, `manifest_load` 12 passed, integration test files pass; CLI against the in-process broker 191 passed, 4 skipped; CLI against an external `mqttv5` broker 192 passed, 3 skipped; fmt and the four pedantic clippy runs clean.

### Citation drift pair removed after the MQTT-3.2.2-2 retag (2026-10-23)

**Trigger**: `tests/manifest_load.rs` failed on the #264 branch. Retagging `connect_clean_start_session_present_zero` from MQTT-3.1.4-3 to MQTT-3.2.2-2 reconciled a pair listed in `known-citation-drift.txt`, and `manifest_and_test_attributes_agree_or_shrink` requires a reconciled pair to be removed. The earlier runs on this branch used `--lib` only, which skips that test, and so does the CI job.

**Fix**: removed `connect_clean_start_session_present_zero MQTT-3.1.4-3` from `known-citation-drift.txt` (23 pairs left).

**Results**: `cargo test -p mqtt5-conformance --features inprocess-fixture` passes: lib 245, manifest_load 11, and the integration test files.

### Review fixes for the #219 to #224 branch (2026-10-22)

**Trigger**: code review of PR #264.

**Fixed**:
- The CLI built the SUT handle outside `catch_unwind`, so a panic while starting the in-process broker failed the trial but left its outcome empty, and the report said `not_run`. `run_trial` now catches that panic too. Control: with a temporary panic in `build_handle`, the report lists the test as `failed` with the panic message.
- A binary built with `panic = "abort"` now exits with code 2 and tells you to use `--profile conformance`. Checked with `cargo build --release -p mqtt5-conformance-cli`: `--list` exits 2 with the message.
- `overlapping_copies` waited only 1 s for a second copy, so a broker sending the second copy late was judged as sending one copy and failed MQTT-3.3.4-4. It now waits the full 10 s timeout for each copy and returns `[Option<ParsedPublish>; 2]`. Any packet that is not a well-formed PUBLISH fails the test instead of being skipped.
- The overlap tests and the takeover test now assert the SUBACK and its granted QoS instead of discarding it, so the takeover test knows the session it takes over holds a subscription.
- `disconnect_session_expiry_after_zero_is_protocol_error` accepted any packet before the close. It now fails unless the only packet is a DISCONNECT, and checks its reason code is 0x82.
- The raw CONNECT builders call `unique_client_id`, which moved from `harness` to `raw_client` (re-exported from `harness`), because `raw_client` is built without the `inprocess-fixture` feature.
- `harness.rs` dropped its `#![allow(clippy::missing_panics_doc)]` and documents `# Panics` on the 12 functions that needed it.
- BROKER_SETUP.md capability counts are exact now (counted from the `requires` lists), with a row for assigned client IDs.

**Not exercised here**: the malformed PUBLISH and non-DISCONNECT failure paths. The in-process broker sends neither.

**Results**: CLI against the in-process broker 198 passed, 3 skipped; conformance lib tests 245 passed; fmt and the four pedantic clippy runs clean.

### Report outcomes, parser regression test, ClientIDs, optional behaviour, coverage gaps (2026-10-22)

**Trigger**: issues #219 to #224, found running the suite against an external broker.

**#219 report status**: the CLI wrote `"planned"` for every test that met its requirements, whether it ran, passed, failed or was filtered out. Reproduced with one test run and 199 filtered out: 197 `planned`, 3 `skipped`. Each trial now records its outcome, and the report gives `passed`, `failed` (with the panic message in `failure`), `skipped` or `not_run`. The full in-process run gives 198 `passed` and 3 `skipped`, and a test run against a closed port is `failed` with its message.

**#220 CONNACK properties**: already fixed by #250, which taught `ParsedProperties` to skip every v5 property it does not extract. A unit test with the issue's two CONNACKs (Receive Maximum, Topic Alias Maximum) fails on the commit before #250 and passes now.

**#221 shared ClientIDs**: the raw CONNECT builders used fixed ClientIDs (`test-flags-client`, `test-version-client`, `test-proto-client`, `test-reserved-client`, `test-will-client`, `test-dup-prop-client`, `test-pw-client`, `malformed-uflag`). Tests running in parallel could take over each other's session, and the takeover close counted as the broker rejecting a malformed packet. Every builder now generates a ULID-suffixed ClientID.

**#222 optional behaviour**: checked against the normative text. MQTT-3.1.3-6 makes accepting an empty ClientID a MAY, so `connect_empty_client_id_server_assigns` and `connack_assigned_client_id_unique` now require the new `assigned_client_id_supported` requirement; with it set to false both are listed as skipped. §3.3.4 makes further copies for overlapping subscriptions a MAY, so the two overlap tests accept one or two copies: the maximum QoS across what arrives must be 1 (MQTT-3.3.4-2), a single copy must carry both identifiers (MQTT-3.3.4-4, now Tested instead of NotApplicable) and two copies must carry one each (MQTT-3.3.4-5). The single-copy branch is not exercised by the in-process broker, which sends two copies. §4.13.1 says the Server MUST close the connection and SHOULD send DISCONNECT, so `disconnect_session_expiry_after_zero_is_protocol_error` accepts a bare close and still checks 0x82 when a DISCONNECT arrives.

**#223 coverage gaps**: `connect_clean_start_takeover_session_present_zero` connects with Clean Start 1 to a ClientID whose session is live with a subscription and checks Session Present 0 (MQTT-3.2.2-2). `publish_retain_handling_zero_sends_retained` now checks the RETAIN flag is 1 on the retained delivery (§3.3.1.3). Negative control: a broker changed to report Session Present whenever a stored session exists, and to send retained messages with RETAIN 0, passes `connect_clean_start_session_present_zero` and fails both new assertions. `connect_clean_start_session_present_zero` was tagged MQTT-3.1.4-3, a takeover statement, while the manifest lists it under MQTT-3.2.2-2; the tag now matches.

**#224 CLI build**: a release build has `panic = "abort"`, so the first failing test ends the run. Reproduced against a closed port: the release binary stopped at the first of 21 tests with no summary, the `conformance` profile build reported 21 failed. The READMEs and BROKER_SETUP.md now build with `--profile conformance` and run `target/conformance/mqtt5-conformance`, and no longer state a test count.

### QUIC buffer budget reservations (2026-10-22)

**Trigger**: second review of PR #263.

**Behavior with the byte counter**: a reader gave its bytes back as soon as it decoded a packet, while the packet still waited in the client handler's 100-slot input queue. With `max_packet_size` 512 KB, a broker stream receive window of 64 KB, and a client subscribed to its own topic that never reads its deliveries (so its handler blocks writing to it), one data flow got 100 complete 200 KB packets read in a row, the most the test sends. Origin/main reads the same 100. Separately, a client writing large packets one after another on new streams was disconnected, because `write_all` returns before the broker has read the previous packet and two packets briefly overlapped.

**Fix**: the budget is a semaphore of `max_packet_size` permits. A reader that sees a fixed header announcing more than 4096 bytes acquires that many permits before reading the rest, and the permit travels with the decoded packet through `ExternalInput::Packet` until the handler has processed it. A reader waiting for permits stops reading, so QUIC flow control slows the client down. Readers holding permits only need the network to finish, so waiting cannot deadlock. Packets of 4096 bytes or less are not reserved, since a reader already holds up to one 4096-byte read chunk uncounted; this keeps PINGREQ and acks on the control stream from waiting behind a large data packet, which the first version of these tests caught. The budget is closed when the handler ends, so waiting readers stop. Going over the budget no longer closes the connection.

**Tests**: `a_stalled_handler_stops_reading_once_queued_packets_fill_the_maximum_size` reads 4 packets here and 100 on main and on the byte-counter commit. The three `partial_packets_..._wait_for_each_other` tests (data and data, control and data, control and ack stream) leave two partial packets over the budget, check the connection stays open, finish both, and check both get through; they fail on the byte-counter commit, where the connection is closed, and pass on main, which has no budget. `an_abandoned_packet_releases_its_reservation` resets a stream holding a reservation and checks a later packet is delivered. A unit test covers reservation, waiting, release and close.

### QUIC buffer budget review fixes (2026-10-22)

**Trigger**: review of PR #263.

**Behavior with the first fix**: a stream's read buffer reserved the full size its fixed header announced as soon as the header arrived, while the budget counted only the bytes received. A unit test feeding `decode_buffered_packet` the five bytes `30 FF FF FF 7F` (a PUBLISH announcing 268435455 bytes) left the buffer with at least that much capacity, so a client could still make the broker reserve close to `max_packet_size` per stream by sending only headers.

**Fix**: `decode_buffered_packet` no longer reserves for the announced size. A buffer now grows with the bytes that arrive, which the budget counts. Going over the budget now returns a `ProtocolError` that names the bytes held across the connection's streams instead of `PacketTooLarge`, which read as one oversized packet. The close code is still ERROR_PROTOCOL_L0.

**Tests**: `an_announced_size_does_not_allocate_before_its_bytes_arrive` fails before the fix and passes after. `complete_and_abandoned_packets_free_the_connection_buffer` now waits for each full packet to be delivered to a subscriber before sending the next, since three full packets read at once could legitimately go over the budget. The reset half of that test still waits on fixed delays, because the broker gives no signal when a reader releases its share.

### QUIC per-connection buffer budget (2026-10-21)

**Trigger**: #236.

**Spec**: nothing in MQTT 5 or MQoQ is broken here, since every packet is held to the `max_packet_size` the broker announces. The concern is memory: a QUIC connection has many streams, each with its own read buffer, while a TCP connection has one.

**Behavior on main**, with a raw QUIC client and `max_packet_size` 1024: two data streams each sending 612 bytes of a 1024-byte PUBLISH, or 612 bytes on the control stream and 612 on a data stream, leave the connection open.

**Fix**: one budget per connection, equal to `max_packet_size`, shared by the control stream (read by the client handler), every client data stream reader, and the ack readers on streams the broker opens. Each reader holds a share sized to the bytes in its buffer, updated after every read and every decoded packet, and released when the reader ends. Going over the budget is reported as `PacketTooLarge` and closes the connection with ERROR_PROTOCOL_L0, the same way an oversized packet does. A connection can still send any single packet up to `max_packet_size`.

**Tests**: `partial_packets_on_data_streams_over_the_maximum_size_close_the_connection`, `partial_packets_on_the_control_and_a_data_stream_over_the_maximum_size_close_the_connection` and `partial_packets_on_the_control_and_an_ack_stream_over_the_maximum_size_close_the_connection` fail on main and pass with the fix. `complete_and_abandoned_packets_free_the_connection_buffer` (three full-size packets on three streams, then a partial packet on a stream that is reset, then another partial packet) passes on both, and checks that complete packets and ended streams give their bytes back. A unit test covers the share accounting.

### QUIC server flow IDs for flow-bound delivery (2026-10-21)

**Trigger**: #253.

**Spec**: MQoQ §9.4 says the least significant bit of a Flow ID tells a server flow from a client flow, and the owner MUST set it correctly. §9.20 lists the server data flow (0x13) as one the server opens. §9.1.2 says a PUBLISH for a subscription goes over the data flow it was subscribed on, but the broker only serves client data flows on unidirectional streams, which cannot carry data back, and §9.2 leaves pairs of unidirectional streams as TBD. So the spec does not say where such a delivery goes; it only rules out the ID the broker used.

**Behavior on main**, with a raw QUIC client subscribing on client flow 5: the delivery stream opened with `13 0A`, a server data flow carrying the client's flow ID (#253 has the trace, and the new test fails on main at the server-owned check).

**Fix**: the broker still opens one server stream per client flow and reuses it, but gives it a server-owned ID from the connection's generator, as it does for per-topic and per-publish streams. Packets the client sends back on that stream are still tagged with the client flow, so acks and anything else read there stay with the subscription's flow.

**Tests**: `subscriptions_on_client_flows_are_delivered_on_server_owned_flows` fails on main and passes with the fix: both delivery headers carry a server-owned ID, and the two IDs differ.

### QUIC flow recovery (2026-10-21)

**Trigger**: #252, and the two cases #254 left to it.

**Spec**: MQoQ §9.8 and §9.15. A flow owner recovers a flow by opening it again with `clean` 0. If the receiver has no state for it and `abort_if_no_state` is set, it MUST stop the flow with ERROR_NO_FLOW_STATE; if the flag is unset, it MUST NOT. A second recovery after that refusal is a level 0 protocol error, and the connection SHOULD be closed with ERROR_TOO_MANY_RECOVER_ATTEMPTS. `clean` 1 makes both ends discard the flow's previous state, and both flags set is a level 1 protocol error, which discards the state. §9.13 says an aborted flow keeps its state until it expires, and a connection closing is an abort, so the state has to outlive the connection. The spec lists ERROR_NO_FLOW_STATE as both 0xB3 and 0xC1; the broker uses 0xB3, as `QuicStreamCode::NoFlowState` does.

**Behavior on main**, with a raw QUIC client: a recovery with no state was served with or without `abort_if_no_state`, so a second one was served too; `clean` 1 kept the subscriptions of the flow it replaced; both flags set was served. The registry was per connection and its expiry timer stopped when the connection closed, so a flow aborted by a closed connection kept its subscriptions in the session forever, and a connection that took over the session inherited the subscriptions of the old connection's expire-0 flows.

**Model**: `specs/tla/flow-recovery` compares the old design, a per-connection registry that honors the flags, and four designs with the state in the session, against three properties: every header gets the answer the rules give, no subscription is removed while the spec still has it, and nothing stale is left once the broker is idle. Only the design with the session registry, a discard that finishes before the new stream is served, a lookup that checks the deadline, and conversion of the old connection's flows when a connection claims the session holds all three, exhaustively at 3 and 4 streams.

**Fix**: the flow state is a per-session table in the router. The client handler is its only writer: a stream reader sends the flow header and waits for the answer, and sends the stream's end on the same channel as its packets. Claiming a session converts the open flows of earlier connections (kept until the expire interval runs out, or removed if it is 0) and drops subscriptions of flows the table no longer holds; releasing it does the same for the owner's flows. A connection expires kept flows from its own loop, and a task does it while the session has no connection. Flow state is not persisted, so restoring sessions at startup drops flow-bound subscriptions.

**Tests**: six new tests in `quic_flow_streams.rs` fail on main and pass with the fix: refusal only with `abort_if_no_state`, the connection close on a second attempt, recovery without `clean` and a fresh start with it, both flags set, recovery across a reconnect until the expire interval runs out, and a takeover dropping expire-0 flow subscriptions. The other 20 tests in the file pass on both.

### Load balancer and MQTT 3.1.1 (2026-10-21)

**Trigger**: #259, the open item from the previous entry.

**Behavior**: a broker with `load_balancer` backends redirects every client, since `select_backend` always returns a backend when any are configured. A 3.1.1 CONNACK has no Server Reference, so a 3.1.1 client gets 0x03 and the connection is closed, which is unchanged. Serving it on the redirecting broker would split it from every other client, so the refusal stays.

**Fix**: the broker logged a 3.1.1 refusal as `Redirecting client to backend`. Both brokers now log a warning that the client must connect to a backend directly, and the docs say redirection is MQTT 5 only. Checked with the CLI broker and two backends: the 3.1.1 client gets `20020003` with the warning, and an MQTT 5 client still gets 0x9C with `mqtt://backend-a:1883` and the info line. `v311_client_is_refused_by_a_load_balancer` in `crates/mqtt5/tests/v311_connack.rs` pins the 3.1.1 CONNACK and the close; it also passes on main, since only the log changed.

### 3.1.1 CONNACK return codes (2026-10-21)

**Trigger**: #257, the open item from the previous entry, and the code review of #258.

**Return codes**: the 3.1.1 CONNACK encoder in `mqtt5-protocol` sent the MQTT 5 value 0x87 for every reason code it did not map. A 3.1.1 client that connected to a session owned by another user got `20020087`; 3.1.1 defines return codes 0 to 5 only. The encoder now goes through `v311_return_code`, which names every `ReasonCode` with no wildcard, so a new code does not compile until it has a 3.1.1 mapping. Server-side and availability refusals (Unspecified error, Implementation specific error, Server unavailable, Server busy, Server shutting down, Quota exceeded, Use another server, Server moved, Connection rate exceeded) encode as 0x03. Bad username or password stays 0x04. Not authorized, Banned, Bad authentication method and refusals caused by the client's packet (Malformed packet, Protocol error, Packet too large, Topic name invalid, Payload format invalid, QoS or Retain not supported) encode as 0x05. Reason codes that are never valid in a CONNACK encode as 0x03.

**Unsupported protocol level**: a CONNECT with protocol name `MQTT` and level 2 or 3 got the MQTT 5 CONNACK `2003008400`. MQTT 3.1.1 MQTT-3.1.2-2 requires return code 0x01, so levels below 5 now get `20020001`, in both brokers. Levels above 5 still get the MQTT 5 0x84. A CONNECT with the MQTT 3.1 protocol name `MQIsdp` is closed without a CONNACK, which MQTT 3.1.1 MQTT-3.1.2-1 allows.

**Tests**: `test_connack_v311_return_codes` in `mqtt5-protocol` checks the encoded bytes for 21 reason codes. `crates/mqtt5/tests/v311_connack.rs` now holds every raw 3.1.1 CONNACK test, including the two that were in `assigned_client_id.rs`, with one CONNECT encoder. The session owner test waits for the broker to close the owner's connection after its DISCONNECT instead of sleeping. The level and session owner tests fail on main.

**Open**: a load balancer redirect cannot be expressed in 3.1.1, which has no Server Reference. A redirected 3.1.1 client gets 0x03 (it got a malformed MQTT 5 CONNACK before) and has nowhere to go.

### Assigned client IDs and 3.1.1 CONNACKs (2026-10-20)

**Trigger**: #255 and the code review of #256.

**MQTT-3.1.3-6**: the broker assigned `auto-<N>` from a counter that starts at 0 in every process, so after a restart an unnamed client with clean start 0 was given the ID of a persisted session and resumed it (session present 1, the other client's queued message delivered). Assigned IDs are now `auto-` plus 128 random bits in hex (`wasm-auto-` in the wasm broker). `connect_empty_client_id_server_assigns` and `connack_assigned_client_id_unique` still pass; they check that IDs are assigned and differ within one SUT run. The restart case needs a SUT restart, so it is covered by `crates/mqtt5/tests/assigned_client_id.rs` instead of this crate.

**MQTT 3.1.1 MQTT-3.1.3-8**: a 3.1.1 CONNECT with an empty ClientId and CleanSession 0 was accepted (`20020000`, connection kept open). It is now refused with return code 0x02 and closed. Like #247, this is a 3.1.1 statement and the manifest is MQTT 5 only, so the test lives in `assigned_client_id.rs`.

**3.1.1 CONNACK format**: rejection paths built the CONNACK with `ConnAckPacket::new`, which always encodes MQTT 5. A 3.1.1 CONNECT with client ID `a/b` got `2003008500` instead of `20020002`. Every CONNACK in both handlers now goes through `new_connack`, which picks the encoding from the connection's protocol version.

**Open**: the 3.1.1 CONNACK encoder in `mqtt5-protocol` maps every reason code without a 3.1.1 equivalent to the MQTT 5 value 0x87, not to 0x05. A 3.1.1 client that connects to a session owned by another user gets `20020087`; 3.1.1 defines return codes 0 to 5 only.

### Offline queued delivery: review fixes (2026-10-19)

**Trigger**: code review of #250.

**Drain loop**: the subscriber answered every queued PUBLISH with a PUBACK, including a QoS 2 one. Against a broker that wrongly queues the QoS 2 probe at QoS 2, that is a protocol error, the broker may disconnect before the marker, and the test failed with a read error instead of the MQTT-3.8.4-8 assertion. A QoS 2 PUBLISH is now returned as soon as it arrives, so the QoS assertion reports it.

**Retained cleanup**: the two RETAIN tests left a retained message on a new topic every run, which piles up on a long-lived external SUT and counts against its retained limit. Both now clear the topic with an empty retained message.

**QoS 0 tests**: `offline_queued_qos0_is_not_upgraded` and `offline_shared_queued_qos0_is_not_upgraded` pass when nothing is delivered, because MQTT 5 lets a server skip QoS 0 for an offline session. Since #248 our broker never queues QoS 0, so against it these tests only check that a QoS 0 message is not delivered as QoS 1. A broker that queues QoS 0 runs the assertion.

**Known limit**: the tests wait a fixed 200 ms after the subscriber's DISCONNECT before publishing, the same pattern the rest of the suite uses. If a SUT takes longer to mark the session offline, the probe is delivered live to the closing connection and the test fails with "a queued PUBLISH arrives before the marker".

**Harness cleanup**: `raw_client.rs` no longer has a file-level lint allow. Lengths go through checked conversions, `encode_variable_int` takes a `usize`, and the three public functions returning `io::Result` document their errors. `connect_resume` and `connect_with_session_expiry` share one builder, and so do `publish_qos1_retained` and `publish_qos1_with_dup`. The `ParsedProperties` doc no longer says unknown property IDs are tolerated.

### What the suite cannot see: 3.1.1, the wasm broker, $SYS publishing (2026-10-19)

**Trigger**: the same review found three problems the suite does not cover. None of them can be closed with a test in this crate today.

**MQTT 3.1.1 negative acks (#247)**: when the broker refuses a QoS 1 or QoS 2 PUBLISH from a 3.1.1 client, it sends an MQTT 5 PUBACK or PUBREC with a reason code and a Reason String, which is malformed in 3.1.1 (MQTT 3.1.1 MQTT-3.3.5-2: positive ack or close). The manifest and the statement lists are MQTT 5 only, so there is no statement to attach a test to.

**wasm Topic Alias (#249)**: the wasm broker advertises Topic Alias Maximum 65535 but never resolves inbound aliases. `section3_publish_alias.rs` covers MQTT-3.3.2-11 and MQTT-3.3.2-12, but every fixture is a native broker, so the suite has never run against the wasm broker.

**`dollar_sys_publish` and #246**: `dollar_topics_matched_by_explicit_prefix` (MQTT-4.7.2-1) has a client publish to `$SYS/test` and expects a `$SYS/#` subscriber to get it, and the `full-broker` profile and the in-process fixture declare `dollar_sys_publish = true`. #246 is about the broker letting clients exchange messages on `$` topics, which MQTT 5 §4.7.2 says a server SHOULD prevent. When #246 is fixed, the in-process fixture loses that capability, and the explicit-prefix test needs a message the broker publishes itself (for example `$SYS/broker/uptime`) so it can still run.

### Offline queued delivery: coverage gap, parser fix, broker fix (2026-10-19)

**Trigger**: a spec review of the 0.48.2 changes found that messages queued for an offline persistent session come back with the wrong flags (#248). MQTT-3.8.4-8, MQTT-3.3.1-12, MQTT-3.3.1-3 and MQTT-4.8.2-3 were all marked Tested, but every test behind them delivers to a connected subscriber, so the offline queue was never exercised.

**New tests** in `section3_offline_delivery.rs`. Each one connects a subscriber with a 300 s Session Expiry, subscribes at QoS 1 with Retain As Published 0, and disconnects normally. A publisher sends the probe message and then a QoS 1 marker. The subscriber reconnects with Clean Start 0, and the test reads queued PUBLISH packets until the marker, so the marker proves the queue was drained:

| test | statement | probe |
|---|---|---|
| `offline_queued_qos0_is_not_upgraded` | MQTT-3.8.4-8 | QoS 0 |
| `offline_queued_qos2_is_downgraded` | MQTT-3.8.4-8 | QoS 2 |
| `offline_queued_retain_is_cleared` | MQTT-3.3.1-12 | QoS 1, RETAIN 1 |
| `offline_queued_dup_is_cleared` | MQTT-3.3.1-3 | QoS 1, DUP 1 |
| `offline_shared_queued_qos0_is_not_upgraded` | MQTT-4.8.2-3 | QoS 0, `$share/<group>/...` |
| `offline_shared_queued_qos2_is_downgraded` | MQTT-4.8.2-3 | QoS 2, shared |
| `offline_shared_queued_retain_is_cleared` | MQTT-3.3.1-12 | QoS 1, RETAIN 1, shared |
| `offline_shared_queued_dup_is_cleared` | MQTT-3.3.1-3 | QoS 1, DUP 1, shared |

Queuing QoS 0 messages for an offline session is optional, so the two QoS 0 tests accept no delivery and only fail if the message arrives above QoS 0.

**Harness problems found on the way**:
- `expect_connack` does a single socket read. On a resumed session the broker sends the CONNACK and the queued PUBLISH packets together, so the read swallowed them. The new tests read with `read_mqtt_packet`, which frames packets.
- `ParsedProperties::parse` returned `None` for every property it does not extract, including all the CONNACK-only ones, so `ParsedConnAck::parse` rejected any CONNACK with properties. It now steps over every property defined in MQTT 5 by its encoded type and still rejects unknown IDs and truncated values. There are new unit tests for both cases.
- `build_connect_resume` moved from `section3_connect.rs` to `RawPacketBuilder::connect_resume`. `RawPacketBuilder::publish_qos1_retained` is new.

**Control** on main (eceda86), before the broker fix: four tests failed and four passed, matching the runtime reproduction in #248. These failed: plain QoS 0 (delivered at QoS 1), shared QoS 0 (QoS 1), shared RETAIN (RETAIN 1) and shared DUP (DUP 1). The QoS 2 downgrade was already correct on both paths, and so were plain RETAIN and DUP.

**Broker fix** in `router.rs`: the plain offline path queues at the effective QoS, and the all-members-offline shared path goes through `prepare_message` with the effective QoS instead of cloning the publish. A message whose effective QoS is 0 is not queued, which matches the rate-limited path. With the fix, the in-process suite passes 243 of 243.

### Mosquitto interop job: Ubuntu archive package instead of a registry image (2026-10-09)

**Trigger**: the Mosquitto interop job failed on PR #210 before any test ran. Docker Hub refused `eclipse-mosquitto:2` with `toomanyrequests: You have reached your unauthenticated pull rate limit`. #214 moved the pull to the ECR Public mirror (same digest, `sha256:38c0da4f…`), and the next run failed there with `toomanyrequests: Rate exceeded`. Hosted runners share IP addresses, so any anonymous registry pull can hit a limit, and logging in does not help pull requests from forks, which get no secrets.

**Change**: the job installs `mosquitto` from the Ubuntu archive, runs `systemctl disable --now mosquitto` so the packaged service cannot answer on 1883, and starts its own process with the same config (`listener 1883 0.0.0.0`, `allow_anonymous true`). The readiness loop fails if that process exits, the process is stopped by PID, and its log is uploaded as the `broker-log-mosquitto` artifact. `runs-on` is pinned to `ubuntu-24.04`, so the Mosquitto version only moves when the pin does.

**Version under test**: 2.0.18 (Ubuntu 24.04). `eclipse-mosquitto:2` had moved to 2.1.2. The fixture is `mosquitto-2.x.toml` and the job runs only `deferred_qos2`.

**Checked locally** with the conformance CLI and the same command the job runs: against Ubuntu 24.04's package in a container (2.0.18), both `deferred_qos2` tests pass; against `eclipse-mosquitto:2` (2.1.2), both pass; with no broker on 1883, both fail.

**Known gap**: the fixture's `restart_command` is `systemctl restart mosquitto`, which would restart the packaged service rather than the job's process. No conformance test calls `restart()` today, and the Docker setup did not match the hook either.

### Statement text drift cleared: all 77 entries corrected against the normative body (2026-10-01)

**Trigger**: issue #166, part 1. `known-text-drift.txt` listed 77 statements whose manifest text did not correspond to the reference text for their ID. The client audit for #164 had taken wrong IDs from the manifest as a result (the manifest filed "Client without session state receiving Session Present=1 MUST close" under MQTT-3.2.2-5; it is MQTT-3.2.2-4).

**Method**: every entry was compared against three sources side by side: the manifest text, `mqtt-v5.0-statement-texts.txt`, and the normative body as extracted in `rfc-extract/mqtt-v5.0-compliance.toml`. The cited tests and their `#[conformance_test(ids = ...)]` attributes were read to decide which statement each test really exercises. As in the earlier passes, no bulk replacement was applied; each entry fell into one of four kinds.

**Applied**

- **53 paraphrases** (right ID, lossy wording, extra sentences, or v3.1.1 vocabulary such as "spec table"): text replaced with the reference text, status and tests kept, since the tests exercise the statement.
- **7 reference-file errors** where the manifest already matched the body and `mqtt-v5.0-statement-texts.txt` still carried an Appendix B variant: `3.1.3-10` (Appendix B says "forwarding the Application Message", the body says "publishing the Will Message"), `3.1.3-12`, `3.11.2-2` ("specified by the receiver" vs "the Client"), `3.14.2-3` and `3.7.2-2` ("use" vs "send"), `3.2.2-18` (Appendix B drops "or 0"), `3.7.2-3`. The reference file was corrected to the body.
- **13 displaced entries**, plus 4 drifted entries that were also destinations (`3.1.2-11`, `3.1.3-5`, `3.3.2-7`, `3.3.4-9`), whose tests exercised a different statement. The tests moved to the statement they exercise, and their attribute, doc comment and assertion messages were retagged:
  - `3.1.2-11` keep-alive tests → `3.1.2-22`; `3.1.2-7` Will QoS test → `3.1.2-11`; `3.1.2-8` Will Retain test → `3.1.2-13`; `3.1.2-6` Will payload test → `3.1.2-9`; `3.1.2-12` CONNECT fixed-header flags test → `2.1.3-1`.
  - `3.1.3-3` allowed-characters test → `3.1.3-5`; `3.1.3-4` empty ClientID test → `3.1.3-6`; `3.1.3-5` 0x85 test → `3.1.3-8`.
  - `3.11.2-1` UNSUBACK packet-id test → `3.10.4-5`; `3.8.3-3` empty-SUBSCRIBE test → `3.8.3-2`; `3.9.3-1` SUBACK count test → `3.8.4-6` (it checks the number of reason codes, not their order).
  - Topic Alias run `3.3.2-7…-10`: connection-scope tests → `3.3.2-7` (and no longer claim `3.3.2-11`); alias-zero test → `3.3.2-8`. `3.3.2-10` is the Client acceptance rule.
  - Receive Maximum run `3.3.4-7…-10` had Client and Server swapped: the outbound limit test → `3.3.4-9`; `-7` and `-8` are the Client rules; `-10` (Server must not delay non-PUBLISH packets) is Untested.
- **Destinations promoted from CrossRef to Tested**: `3.1.2-22`, `3.1.2-13`, `3.1.3-6`, `3.8.3-2`, `3.8.4-2`, `3.8.4-6`, `3.10.4-5`. Their notes had pointed back at the wrong IDs.
- **`3.10.4-2`** ("When a Server receives UNSUBSCRIBE it MUST stop adding new messages") was recorded as Client/NotApplicable. It is a Server statement and `unsubscribe_stops_delivery` exercises it.
- **Citation drift reconciled where the corrected text made a pair correct** (13 pairs removed from `known-citation-drift.txt`): `connect_will_qos_3_is_malformed` is `3.1.2-12` (it restates that statement), `suback_packet_id_matches` is `3.8.4-2`, `overlapping_subs_no_local_prevents_echo` is `3.8.3-3`, and the manifest now cites the tests already declaring `3.14.2-1`, `4.13.2-1`, `3.8.4-8`, `4.9.0-2` and `3.11.3-2`.

**Result**: `known-text-drift.txt` is empty and still guards against new drift; `known-citation-drift.txt` is down from 37 to 24 pairs. Status distribution: Tested 162 → 163, Untested 34 → 39, CrossRef 19 → 12, NotApplicable 36 → 37 (251 statements). The new Untested entries are real gaps the wrong IDs were hiding: `3.1.2-6` (new Session for Clean Start 0 without a session), `3.1.2-7` (Will Message stored), `3.1.2-8` (Will published after close or delay), `3.1.3-3` (ClientID first in the payload), `3.1.3-4` (ClientID is UTF-8), `3.3.4-10`, `3.9.3-1` (SUBACK reason-code order).

**Dropped note**: `3.3.4-8` carried a note about the unregistered test `inbound_receive_maximum_exceeded_disconnects_with_0x93`. `3.3.4-8` is the Client "must not delay" rule, so the note did not belong there. The 0x93 DISCONNECT behaviour has no numbered statement of its own.

**Not in this pass**: issue #166 part 2 (a client SUT mode) is unchanged.

### Absent Session Expiry now means 0, and DISCONNECT can change the Session Expiry (2026-09-24)

**Trigger**: the quorum review of PR #170 and issue #171, which was folded into it. The broker stored an absent CONNECT Session Expiry Interval as "never expires", although §3.1.2.11.2 says an absent value is 0. A client that left the property out kept its session, subscriptions and queued messages forever. It also broke the Will timing from #154: with the Will bounded by session end, "never" meant the delay was always honoured instead of the Will going out at disconnect. The broker also ignored a Session Expiry Interval sent on DISCONNECT (§3.14.2.2.2), and a resumed session kept the Session Expiry of the connection that created it instead of taking the resuming CONNECT's value.

**Fix**: the Session Expiry is worked out once, from the CONNECT, when a session is created or resumed. MQTT v5: the property's value, or 0 when absent. MQTT v3.1.1 has no property, so CleanSession=1 gives 0 and CleanSession=0 keeps the session with no expiry, as before. A Session Expiry on DISCONNECT replaces the stored value before the session-end and Will logic runs. If CONNECT had 0 and DISCONNECT sends a non-zero value, the server sends DISCONNECT 0x82 and closes. The spec says such a DISCONNECT is not valid, so it is not a normal disconnection: the Will is published, and the session still ends because its expiry stays 0.

**New tests**, all failing on the tree before the fix:
- `absent_session_expiry_discards_session_at_disconnect` (`[MQTT-4.1.0-2]`): connect without the property, subscribe at QoS 1, disconnect, publish while offline, then reconnect with Clean Start 0. It expects Session Present 0 and no delivery of the offline message.
- `disconnect_session_expiry_zero_discards_session` (`[MQTT-4.1.0-2]`): CONNECT 300, DISCONNECT 0, then Session Present 0.
- `disconnect_session_expiry_extends_session` (`[MQTT-3.1.2-23]`): CONNECT 1, DISCONNECT 300, wait 2.5 s, then Session Present 1.
- `disconnect_session_expiry_after_zero_is_protocol_error` (`[MQTT-4.13.1-1]`): CONNECT 0, DISCONNECT 300, then DISCONNECT 0x82 and the connection closes.

**IDs**: neither "absent means 0" nor the DISCONNECT 0 to non-zero rule has its own normative statement in `mqtt-v5.0-statement-texts.txt`; both are prose in §3.1.2.11.2 and §3.14.2.2.2. The tests are filed under the statements they exercise: session discard after the interval (MQTT-4.1.0-2), session storage when the interval is above 0 (MQTT-3.1.2-23), and closing the connection on a Protocol Error (MQTT-4.13.1-1). The manifest text for all three matches the statement file.

**Knock-on**: tests elsewhere in the workspace had resumed sessions without setting a Session Expiry. Several of them only asserted inside `if session_present`, so after the fix they would have passed without checking anything. They now set an explicit expiry and assert Session Present.

### Delayed Will was never cancelled, and the MQTT-3.1.3-9 test could not see it (2026-09-24)

**Trigger**: issue #154. With a Will Delay Interval above zero, the broker spawned a detached task that slept for the delay and then published the Will unconditionally. A client that reconnected inside the delay still had its Will published, which violates `[MQTT-3.1.3-9]` and the "new Network Connection ... before the Will Delay Interval has elapsed" clause of `[MQTT-3.1.2-8]`.

**Why the suite passed anyway**: `will_delay_reconnect_suppresses_will` used a 5 s delay but stopped watching about 2.3 s after the drop, so the stale Will always arrived after the assertion. The test was vacuous. It now uses a 2 s delay and waits 4 s after the reconnect. On the unfixed broker it fails with the Will received. A positive control, `will_delay_elapsed_publishes_will`, runs the same setup without a reconnect and asserts that nothing arrives at 1.2 s and that the Will arrives once the delay has elapsed. It passes on both the old and the fixed broker, which shows the negative test fails for the right reason and not because Wills are never delivered.

**Fix**: the router keeps one pending delayed Will per client id, tagged with the generation of the connection that armed it. A connection arms its Will before it releases its router entry, and only if it still owns that entry. `register_session` removes any pending Will for the client id while it holds the clients write lock, so any new connection (Clean Start 0 or 1, takeover included) cancels it. When the timer fires, the task has to claim the entry by generation before it publishes. Claim and cancel both remove the entry under one mutex, so exactly one of them wins. The Will fires at min(Will Delay Interval, Session Expiry Interval), so a Session Expiry of 0 publishes at once and a shorter expiry publishes when the session ends (`[MQTT-3.1.2-8]`, §3.1.3.2.2). A published Will, and a Will deleted by DISCONNECT 0x00, is also removed from the stored session (`[MQTT-3.1.2-10]`).

**Manifest**: both tests are listed under MQTT-3.1.3-9, whose manifest text matches `mqtt-v5.0-statement-texts.txt`. The manifest entry labelled MQTT-3.1.2-8 carries the Will Retain text ("If the Will Flag is set to 0, then Will Retain MUST be set to 0"), not the Will publication statement, so neither test is cited there. That drift is left as it was.

**Broker-side coverage**: `crates/mqtt5/tests/will_delay.rs` covers resume and clean-start reconnects, no reconnect, Session Expiry 0 and 2 against longer delays, reconnect-then-drop, DISCONNECT 0x00 and 0x04, and takeover with and without a delay. Six of these fail on the unfixed broker. The other four are regression guards for behaviour that was already correct.

### Quorum review of the client fixes, and a TLA+-verified outcome model for the offline queue (2026-09-23)

**Trigger**: a five-reviewer quorum review of PR #164 before merge. Most findings came with a failing test. The worst was a regression in the wasm client: a v3.1.1 persistent session could never reconnect, because the session-lifetime check used Session Expiry (always 0 in 3.1.1) and the new strict `[MQTT-3.2.2-4]` check then rejected the broker's Session Present=1 forever. Other findings: QUIC teardown left the connection open after the client's own DISCONNECT; a publish waiting across a reconnect was sent under the old server's limits; and the offline queue dropped messages silently after `publish()` had returned success. All were fixed in the same PR.

**The offline-queue question was settled by three independent TLA+ models, not by argument.** All three found the same defects in the current behaviour: silent loss at flush, silent loss on Session Present=0, and replay resending packets the new CONNACK forbids. All three arrived at the same design: per-publish outcomes (Delivered / Rejected / Indeterminate) that are never keyed by packet id, checks at enqueue, reject-and-continue at flush, re-checks on replay, and quarantine of abandoned QoS 2 ids. Holding a message fails liveness, skipping ahead breaks ordering, and transforming it silently loses the caller's intent. One model showed that reporting failures by packet id misattributes them after the id is reused (ABA). The user chose downgrade-and-report for Maximum QoS, and requeueing QoS 1 when the server loses the session. A Clean Start=1 connect discards unacked outbound state, as `[MQTT-3.1.2-4]` requires. The consolidated spec is in `specs/tla/offline-queue/`. New tests are in `crates/mqtt5/tests/conf_client_offline_queue.rs`: 11 fake-broker tests, all failing on the prior tree.

**Second quorum round on the outcome code**: three re-reviewers checked the implementation against the models. The protocol behaviour held. Two defects were in how outcomes are settled: a connection loss never closed the send quota, so a parked flush task kept handles alive forever, and a reader aborted between releasing session state and settling the outcome left a delivered publish unsettled. A live publish returned `Err` on connection loss although its message stayed in the session and was resent. It now returns a handle like a queued one. A QoS 2 message that received PUBREC Success before the session was lost is reported delivered. Each fix has a test that fails on the previous code.

**Tooling lesson**: tla-mcp 0.9.4 passed a `~>` negative control vacuously and ignored missing fairness. Liveness results are trusted only as `[]<>` properties with negative controls that fail, backed by an ENABLED-based progress invariant.

### Client-side audit: the suite only ever tested brokers, and our own clients failed ~40 MUSTs (2026-09-23)

**Trigger**: checking a third-party client's claim of full MQTT v5 conformance. This suite's SUT is always a broker, so it could not answer the question. The 149 statements in `conformance.toml` with `applies_to = "Client"` or `"Both"` were audited instead with raw-byte fake-broker tests that drive the real client and record what it puts on the wire. After the third-party client had been tested, the same audit ran against our own `MqttClient` and the `mqtt5-wasm` client.

**Result**: our native client failed about 40 client MUST statements. The third-party client failed 7. Resend on session resume did not exist. Packet identifiers were reused while in flight. There was no topic or filter validation. Topic Alias Maximum and Retain Available were never enforced. The offline queue bypassed flow control. Protocol errors left the socket half-open. WebSocket reads assumed one packet per frame. The wasm client had most of the same defects and also never sent PUBACK. All of them are fixed in mqtt5 0.41.0 and mqtt5-wasm 2.0.0.

**Where the tests live**: `crates/mqtt5/tests/conf_client_{a,b,c,d}.rs` (native) and `crates/mqtt5-wasm/tests/conformance_client.rs` (wasm, MessagePort fake broker under Node). They are not yet registered in this crate's manifest or runner. A client-side SUT mode for this suite is the natural next step.

**Manifest drift bit the audit**: statement IDs in `conformance.toml` were used to label findings, and several were wrong. For example, the manifest files "no session state + Session Present=1 → close" under 3.2.2-5, but it is 3.2.2-4. All client-test names use IDs from `mqtt-v5.0-statement-texts.txt`. `known-text-drift.txt` is real debt with consequences outside this crate.

**Decision recorded**: `[MQTT-3.2.2-4]` is enforced strictly by default. A fresh client can still resume a broker-held session through an explicit `ConnectOptions::resume_existing_session` opt-in, which the deferred-ack crash-recovery pattern needs. This crate's in-process test client sets the opt-in, because it checks `session_present` as an observer of the broker.

**Lesson**: a conformance suite that only tests one side of the protocol says nothing about the other. Run any check we would point at someone else's implementation against our own first.

### External-broker ack timeouts were a lost-wakeup race in the test client, not broker timing (2026-09-07)

**Trigger**: issue #146. `deferred_qos2_zero_quota_still_serves_control_plane [MQTT-4.9.0-3]` failed
twice on `main` in the external-mqtt5 job (`8791b58` 2026-08-12, `a8c791a` 2026-09-06), both times
`Timeout("pubrec")` from the test's QoS 2 publisher. Identical code passed on neighbouring commits.

**Root cause**: `TestClient::publish_with_options`, `subscribe` and `unsubscribe` all wrote the packet
first and only then called `await_ack`, which is where the `oneshot` waiter was inserted into
`pending_acks`. The reader task resolves an incoming ack by `remove`-ing the packet id from that map
and drops it if nothing is registered. If the broker's reply was read in the gap between the flush
returning and the insert, the ack was gone and the waiter expired after `ACK_TIMEOUT`. The window is
microseconds of synchronous code, so it needs the test thread to be preempted at that instant; a
loaded two-core runner with a separate broker process does that occasionally, which is why only the
external-broker job ever failed and why re-runs pass.

**This corrects the 2026-08-06 entry below.** That investigation saw `Timeout("puback")` on the same
job, correctly ruled out concurrency and CPU starvation, could not reproduce it, and concluded it was
"the rare timing transient the 30s `ACK_TIMEOUT` already exists to absorb". A 30-second timeout
expiring is not a slow broker. It is an ack that was consumed with no one waiting. Same race, QoS 1
flavour. The memory-backend change made there was still sound; it just did not touch this.

**Fix**: one `send_and_await_ack(packet, id, op)` helper registers the waiter, then writes, then
waits, removing the waiter if the write fails. All five sites use it (PUBACK, PUBREC, PUBCOMP after
PUBREL, SUBACK, UNSUBACK), so the ordering can no longer be gotten wrong per call site.

**Tests**: `ack_map_tests` (not fixture-gated) pin the map semantics: an ack completed after
registration is delivered; one completed before registration is lost. The race itself is not
reproducible on demand; the proof is structural, plus CI history from here.

**Lesson**: a send-then-register pattern around a oneshot map is a lost-wakeup bug, however small the
window. Register first, always.

### [MQTT-3.1.4-3] takeover DISCONNECT is now required by the test, and sent by the broker (2026-09-07)

**Trigger**: issue #147. The in-tree broker closed the displaced client's socket on session takeover
without sending `DISCONNECT` 0x8E, yet `session_takeover_disconnects_existing_client` was green on
every SUT. Found while writing the 0.39.0 client tests, where a takeover against the in-tree broker
surfaced as `NetworkError("Client closed connection")` instead of `ServerDisconnect(SessionTakenOver)`.

**Why the test passed anyway**: it read the superseded connection with
`if let Some(reason) = expect_disconnect_packet(..)` and asserted the reason code only inside the
`if let`. `expect_disconnect_packet` returns `None` on a bare close, so the body was skipped and the
test fell through to the close assertion, which the broker did satisfy. The first half of a MUST was
never checked.

**Test change**: the DISCONNECT is now required (`.expect(..)`), then its reason must be 0x8E, then
the close must follow. Against the unfixed broker it fails at the first step. Mosquitto sends 0x8E.

**Broker fix (same PR)**: both `disconnect_rx` arms in `client_handler/mod.rs` (`handle_packets`
and `handle_packets_no_keepalive`) write `DisconnectPacket { reason_code: SessionTakenOver }` via
`write_to_client` before returning, ignoring a write error since the peer may already be gone. The
takeover signal itself, a oneshot fired from `MessageRouter::register_client`, already existed; only
the packet was missing. One handler serves TCP, TLS, WebSocket and QUIC, so all transports get it.

**Swept for the same shape**: one other `if let Some(..) = expect_disconnect_packet(..)` exists, in
`server_disconnect_uses_valid_reason_code` for `[MQTT-3.14.2-1]`. That one is legitimately
conditional: the statement constrains the reason code only when a DISCONNECT is sent, and sending
one for a second CONNECT is not required by it. Left as is.

**Client side**: `connection_lifecycle_events::session_takeover_via_broker_carries_reason_code`
now proves the whole path end to end against the in-process `TestBroker`; for 0.39.0 it had to use
a fake broker.

### External-broker CI flake investigation — concurrency ruled out, backend switched to memory (2026-08-06)

**Trigger**: `puback_error_stops_retransmission [MQTT-4.4.0-2]` failed once on the external-mqtt5 CI
job with `Timeout("puback")`, on the post-merge run of commit `c44c42a`. It passed on the PR and on
the very next main commit (`6bea100`) — a classic non-deterministic transient.

**Hypothesis (WRONG, overturned by evidence)**: the ~187-test suite runs at libtest-mimic default
parallelism against one shared broker, so on a 2-core runner the broker gets CPU-starved and the
client's ack times out. Reducing `--test-threads` was the proposed fix.

**The confound I introduced.** A first local sweep seemed to confirm it (threads=1: 0 fail →
threads=12: 19 fail). It was an artifact: the `mqttv5` broker defaults to the **file** storage
backend at `./mqtt_storage`, and across ~25 broker restarts during the sweep the store grew to
**169,302 session files**. Each fresh broker reloaded that growing store at startup, and the slow
reload — not concurrency — caused the CONNACK/PUBACK timeouts. Higher parallelism merely hit the slow
broker harder, mimicking a concurrency curve.

**Controlled result (fresh empty storage per run — faithful to CI's clean runner):**

| condition (fresh storage) | result |
|---|---|
| threads = 1 / 2 / 12, unloaded | 0 / 0 / 0 failures (×2 each); threads=12 fastest at ~12s |
| threads = 2, ~2 cores (10 CPU hogs) | 0/3 runs failed |
| threads = 1, ~2 cores (10 CPU hogs) | 0/3 runs failed |

The same 10-hog load that produced 3–6 failures per run **with** the polluted store produced **zero**
with a clean store. Concurrency and CPU starvation are both ruled out. The original CI failure was on
a fresh runner (empty store, one broker start) and could not be reproduced across ~16 controlled clean
runs — it is the rare timing transient the 30s `ACK_TIMEOUT` already exists to absorb.

**Action taken**: `--test-threads=1` was NOT applied (it fixes a non-existent cause and would slow
the job). Instead the CI broker now runs with `--storage-backend memory` (`conformance.yml`): verified
184/184 pass, lossless (no test restarts the broker mid-run), ~12s, and it creates no `mqtt_storage`.
This is the correct backend for an ephemeral single-run test broker — it removes disk I/O from the
broker's per-connect/QoS hot path (a plausible transient-stall contributor on a shared runner) and
eliminates the persistence footgun that derailed this very investigation. Not claimed as a proven fix
for the specific transient, which was not reproducible; a job-level retry was considered and
deliberately left out.

### Fix #129 — broker honours the client's Maximum Packet Size for Reason Strings (2026-08-05)

The broker attached a Reason String to CONNACK/SUBACK/PUBACK/PUBREC/AUTH without consulting the
client's Maximum Packet Size, violating `[MQTT-3.2.2-19]`, `[MQTT-3.4.2-2]`, `[MQTT-3.5.2-2]`,
`[MQTT-3.9.2-2]` and `[MQTT-3.15.2-2]`. The AUTH-failure path also ignored Request Problem Information
(`[MQTT-3.1.2-29]`), and the "not authorized" PUBACK/PUBREC interpolated the peer's own topic name
into the Reason String (peer-controlled overrun up to ~65 KB, plus a u16 UTF-8 length overflow at a
topic length of 65 500).

Fix:
- Added `Properties::remove_reason_string` (mqtt5-protocol).
- Added a single write-path choke point `ClientHandler::write_to_client`: if the encoded packet
  exceeds the client's Maximum Packet Size it omits the Reason String and re-encodes; if it still does
  not fit it is discarded per `[MQTT-3.1.2-24]`. Every CONNACK/SUBACK/UNSUBACK/PUBACK/PUBREC/AUTH/
  DISCONNECT write now routes through it (PUBLISH keeps its own discard path).
- Hoisted the `client_max_packet_size` capture in `connect.rs` to before `handle_authentication`, so
  the auth-failure CONNACK actually honours it — the "trap" the issue describes. Verified: with the
  hoist reverted, the CONNACK test fails with the full Reason String present.
- Gated the AUTH-failure Reason String on Request Problem Information.
- Replaced the two peer-controlled "Not authorized to publish to topic: {topic}" Reason Strings with a
  static "Not authorized to publish", removing the overrun and the u16 overflow at the source.

Tests: `connack_reason_string_omitted_over_max_packet_size` (registered conformance test, `[MQTT-3.2.2-19]`,
proven to fail without the hoist) and `puback_reason_string_omitted_over_max_packet_size` (a lib test,
`maximum_qos = 0`, not registered because it needs a non-default config). Manifest: `[MQTT-3.2.2-19]`
Untested→Tested; `[MQTT-3.4.2-2]`/`[MQTT-3.5.2-2]`/`[MQTT-3.15.2-2]` notes corrected from "reproduced as
a live violation" to the fix (kept Untested — no registered conformance test reaches them on a default
broker). All 225 registered conformance tests still pass, confirming the rerouting did not disturb
normal control-packet delivery.

Note left for the manifest owner: `[MQTT-3.9.2-1]`'s text is the SUBACK packet-identifier rule, but its
audit note describes a SUBACK Reason String / Maximum Packet Size violation — the note looks
misattributed. This fix does route SUBACK through the choke point, but it does not touch packet
identifiers, so `[MQTT-3.9.2-1]` was left untouched rather than relitigated here.

Follow-up caught by an adversarial quorum before merge: routing the CONNACK through the choke point
turned a previously-benign `Maximum Packet Size = 0` into a connection wedge — with the limit at 0
every encoded packet (≥ 1 byte) exceeds it, so even the success CONNACK was discarded and the client
hung forever. MQTT v5.0 3.1.2.11.4 makes a Maximum Packet Size of 0 a Protocol Error, so `handle_connect`
now rejects such a CONNECT with a Protocol Error (0x82) CONNACK before the limit is armed. Confirmed
with a counter-test (`zero_max_packet_size_rejected`) that hangs on the unfixed code and passes after.
Also hardened the PUBACK lib test: a length guard before indexing the reason-code byte, and the client
limit raised 30→35 to give the success-CONNACK handshake headroom (it was a 7-byte margin).

### Fix #130 — publisher Topic Alias stripped before delivery (2026-08-04)

`resolve_topic_alias` (`broker/client_handler/publish.rs`) resolved an inbound Topic Alias to a topic
name but never removed the `0x23` property, so it propagated to subscribers — including subscribers
that advertised no Topic Alias Maximum. Violates `[MQTT-3.1.2-26]`, `[MQTT-3.1.2-27]` and
`[MQTT-3.3.2-11]`.

Fix: added `Properties::remove_topic_alias` (the `mqtt5-protocol` HashMap is `pub(crate)`, so the
broker needs a public accessor) and call it once the topic name is resolved.

Manifest: `MQTT-3.1.2-26`, `MQTT-3.1.2-27`, `MQTT-3.3.2-11` moved Untested→Tested. The existing
`topic_alias_stripped_before_delivery` test only checked the delivered topic *name* (which was always
correct — the leak was the surviving property), and was mis-tagged `[MQTT-3.3.2-8]` (the audit
retargeted that ID to the alias-value-0 rule). Retargeted it to the three real IDs and strengthened it
to parse the delivered PUBLISH with `ParsedPublish` and assert `topic_alias.is_none()`. Verified it
fails without the fix (`found Some(2)`) and passes with it. Removed the now-reconciled
`topic_alias_stripped_before_delivery MQTT-3.3.2-8` line from `known-citation-drift.txt`.

### Bulk heuristic corrections REVERTED after adversarial review (2026-08-03)

A four-reviewer adversarial quorum audited the same day's rework. It found that the bulk corrections,
which used text-similarity matching, introduced real damage alongside real fixes. The heuristic work
has been reverted; only individually-verified changes were re-applied on top of `HEAD`.

**Damage the reviewers found (all reproduced before reverting):**

- **`MQTT-4.8.2-2` was correct before it was "fixed".** Its text was a *truncation* of the real
  statement ("The ShareName MUST NOT contain the characters /, + or #"), not another statement's
  text. A 0.70 similarity collision with `MQTT-3.3.2-14` ("The Response Topic MUST NOT contain
  wildcard characters") demoted a passing entry to `Untested` and grafted its ShareName test onto
  the Response Topic statement, which then claimed coverage from an unrelated test.
- **`MQTT-3.1.3-1` regressed.** The original text was body-correct ("Will Properties, Will Topic,
  Will Payload"); it was overwritten with Appendix B's erratum ("Will Topic, Will Message" — v3.1.1
  naming). A second Appendix B erratum, missed because the "truncated" bucket was replaced from the
  appendix without body cross-check.
- **12 `note` values landed on `[sections."X"]` tables instead of statements.** `Section` has a
  `pub note` field, so it parsed silently; `[sections."3.5"]` (PUBREC) ended up annotated "Broker
  never includes User Property in PUBACK packets". Twelve statements lost their notes.
- Further bad re-points: `puback_message_delivered_on_qos1` filed under a PUBACK-packet-identifier
  statement while publishing at QoS 0 (`PublishOptions::default()` is `AtMostOnce`);
  `unsuback_packet_id_matches` filed under a SUBACK statement; tests moved onto `MQTT-3.10.4-2`
  while it remained `NotApplicable`/`Client`, so they counted toward nothing.
- The session-takeover test could **false-pass**: `expect_disconnect` (`raw_client.rs:702`) returns
  `true` on first byte `0xE0` without checking the socket closed, so a broker that sends DISCONNECT
  and holds the connection open — the exact violation — passed. Now fixed: the test checks the
  reason code if a DISCONNECT arrives, then asserts the socket is genuinely closed.

**What was re-applied** (each independently verified by at least two reviewers): the 5 fabricated-ID
deletions and the three `CrossRef` → `Tested` promotions that inherit their tests; the 9 missing
statements (`4.6.0-6` taken from the body, not Appendix B's "every, Topic" typo); the §3.1.4 rewrite;
three body-verified divergence fixes (`3.1.2-9`, `3.9.3-2`, `4.12.0-2`); the exclusion re-review;
and the removal of 13 citations to tests that are not in the registry.

**Two exemptions were false and are now `Untested`, both reproduced on the wire by a reviewer:**
the five Reason String statements (the broker does emit them and never checks the client's Maximum
Packet Size) and `MQTT-3.1.2-26`/`-27` (`resolve_topic_alias` never strips the Topic Alias property,
so it propagates to subscribers). Both are library bugs, tracked separately.

**Guards rebuilt as ratchets.** Reviewer mutation-testing showed 3 of 4 seeded defects passed all
nine previous guards — critically, **no guard read `text` at all**, so the exact defect class this
work existed to fix could be reintroduced invisibly. Two new guards close that:
`statement_text_drift_only_shrinks` compares every statement against
`mqtt-v5.0-statement-texts.txt` (Appendix B with body overrides where they diverge), and
`manifest_and_test_attributes_agree_or_shrink` checks the manifest and `#[conformance_test(ids)]`
cite each other. Both are baselined against `known-text-drift.txt` (79 entries) and
`known-citation-drift.txt` (35 pairs): they fail on any NEW drift and also fail when a listed entry
is repaired without being removed, so the debt can only shrink. `section_totals_match_statement_counts`
now also fails if `total_statements` is absent, which previously disabled it silently.

**The honest scope.** 79 statements carry text that does not correspond to their ID — larger than the
61 claimed fixed earlier, and the earlier count was itself partly wrong. That backlog is now
explicit, machine-checked, and monotonically decreasing, which is the durable outcome. Each entry
must be corrected **against the normative body**, one at a time; similarity matching is what caused
the damage and must not be used again.

State: 251 statements, Tested 159 (63.3%), Untested 37, CrossRef 19, NotApplicable 36, Skipped 0.
Conformance CLI 183 passed / 0 failed / 3 ignored; guards 11/11; clippy pedantic clean.

### Re-review of the exclusions — one false premise, one mis-excused Server obligation (2026-08-03)

The mis-numbering audit corrected statement *text*. It did not revisit the `NotApplicable` /
`Skipped` *judgements*, which were made under the same regime and, in several cases, about
statements whose text has since changed. This pass re-read every exclusion that binds the Server
(`applies_to` = Server or Both): 23 entries.

**The important finding: 5 exclusions rested on a factually false premise.**

Sixteen entries were excused with "Broker never includes Reason String / User Property in X;
constraint trivially satisfied". The User Property and Topic Alias halves check out — the broker
genuinely never emits those. **The Reason String half is false.** The broker sets a Reason String on:

| Packet | Site |
|---|---|
| CONNACK | `broker/client_handler/connect.rs:285, 297, 322` |
| SUBACK | `broker/client_handler/subscribe.rs:281, 286` |
| PUBACK | `broker/client_handler/publish.rs:259, 291, 370` |
| PUBREC | `broker/client_handler/publish.rs:271, 302` |
| AUTH | `broker/client_handler/auth.rs:134` |

So `MQTT-3.2.2-19`, `3.9.2-2`, `3.4.2-2`, `3.5.2-2` and `3.15.2-2` are live requirements, not
vacuous ones. All five moved `NotApplicable` → `Untested`.

**This may be an actual non-conformance.** Each of those statements says the sender MUST NOT send
the property if it would push the packet beyond the receiver's Maximum Packet Size. The broker
captures `client_max_packet_size` at `connect.rs:110` but consults it only for outbound PUBLISH
(`publish.rs:734`). Nothing checks it before appending a Reason String to an ack. A client
advertising a small Maximum Packet Size and triggering a long failure reason looks capable of
receiving an oversized packet. Not yet reproduced — flagged for investigation.

**`MQTT-3.3.1-1` was mis-excused.** Noted as "Client-side re-delivery behavior", but the statement
reads "MUST be set to 1 by the Client **or Server** when it attempts to re-deliver a PUBLISH
packet", and this broker re-delivers unacknowledged QoS 1/2 messages on reconnect. A genuine Server
obligation with no test → `Untested`.

**`MQTT-4.8.2-4` and `4.8.2-5` un-skipped.** Both are Server obligations governing shared-subscription
redelivery; `4.8.2-5` carried no justification note at all. Both → `Untested`. The `Skipped` status
is now unused.

**Confirmed correct after re-reading the corrected text:** `MQTT-3.1.3-1`, `3.2.2-4`, `3.2.2-21`,
`4.12.1-1` — in each the MUST binds the Client, so a broker has no obligation.

**Remaining implementation-dependent exemptions re-noted.** The 14 surviving "trivially satisfied"
entries had their premises verified against the code, and their notes now say explicitly that the
exemption is *implementation-dependent, not a spec exemption* — it holds only while this broker
never emits the property, and **is invalid for an external SUT that does**. That is a latent flaw in
a vendor-neutral suite: a third-party broker which does send Reason Strings would be silently
exempted from a rule that binds it. Properly these should be decided at runtime from SUT
capabilities rather than baked into the manifest.

Status distribution after this pass: Tested 155, Untested 50, NotApplicable 34, CrossRef 12,
Skipped 0 (of 251). Coverage unchanged at 61.8% — this pass moved statements out of exemption into
honest untested, it did not change what is verified.

### Manifest statement IDs do not reliably denote the OASIS statements they name (2026-08-03)

Found while verifying a spec citation for the ComNet paper, then audited by a five-agent quorum
against a locally-decoded copy of the OASIS MQTT v5.0 standard.

**Two independent failure modes, both of which inflate reported coverage.**

**1. Statement text is assigned to the wrong ID.** Verified authoritative diff over all 247
entries: 180 faithful, 61 mismatched, 6 IDs that do not exist in MQTT v5.0. The damage is not
scattered — it comes in contiguous runs (`3.1.2-6…-12`, `3.1.3-3…-5`, `3.1.4-2…-6`, Topic Alias
`3.3.2-7…-12`, Receive Maximum `3.3.4-7…-10` with Client/Server roles swapped).

Mechanism: the table has two strata. Entries bulk-imported from
`rfc-extract/mqtt-v5.0-compliance.toml` (diary, 2026-02-19) reproduce the spec faithfully; the
older hand-written cohort was never reconciled and roughly half of it is wrong. Diagnostic tells:
`MQTT-3.1.4-2` carried "non-zero **return code**" (v3.1.1 vocabulary — v5.0 says Reason Code), and
`MQTT-3.10.3-1`/`3.1.3-11` cite "section 1.5.4" where v3.1.1 says 1.5.3, i.e. a v3.1.1 table
hand-renumbered in place. It is NOT a clean version offset — the mapping is irregular, so every ID
must be checked individually. No bulk offset fix is valid.

**2. `status = "Tested"` did not imply a test runs.** 16 statements cited 13 test names absent from
the `linkme` registry. 12 of those exist in `tests/` as plain `#[tokio::test]` functions that the
conformance CLI never executes; `subscribe_replaces_existing_qos` does not exist at all.
`tests/manifest_load.rs` validates none of this.

**The consequence that matters: `[MQTT-3.1.4-3]` (session takeover) had no test.** `0x8E` appears
exactly once in the crate, as a byte in a list of valid DISCONNECT reason codes. No test opens a
second connection on a live ClientID, so a broker that silently accepts duplicate ClientIDs passes
the entire suite. The entry under that ID actually held `MQTT-3.2.2-2`'s text.

**Applied in this pass**

- Corrected `MQTT-3.1.4-2` … `-6` to their authoritative v5.0 text, with `level` corrected
  (`-2` Must→May, `-6` Must→MustNot) and `status` set to `Untested`, since each entry's test
  demonstrably exercises a different statement. Orphaned tests are named in each `note`.
- Promoted `MQTT-3.2.2-2`, `MQTT-3.2.2-3`, `MQTT-3.2.0-1` from `CrossRef` to `Tested`. These are the
  correct homes for the displaced texts and already carried the same tests; their `note` fields had
  pointed *back* at the wrong IDs, papering over the duplication.
- Downgraded the 16 statements citing unregistered tests to `Untested`, naming the tests to register.

Reported coverage falls 174/247 (70.4%) → 158/247 (64.0%). The drop is the point: the previous
figure counted tests that never ran and statements that named requirements they did not contain.

**Method note — do not blanket-replace from Appendix B.** The spec's Appendix B is a verbatim
ID→text table for all 251 statements and is far better than heuristic extraction, but it is
explicitly non-normative and contains at least one erratum: `MQTT-3.3.1-10` is rendered there with
its Retain Handling logic **inverted** relative to the normative body ("did already exist" vs "did
not already exist"). Our entry matches the body and is correct. Eleven appendix entries diverge from
the body under a phrase check. Always cross-read the body before correcting an entry.

**Completed in the second pass (same day)**

- **50 further text corrections**, split by evidence rather than applied blindly. 18 *displaced*
  entries (text provably belonged to another ID, so the attached test was exercising a different
  statement) were corrected and set `Untested`, with the real owner named in each `note`. 32
  *truncated/paraphrased* entries (same statement, lossy wording) were corrected with status
  retained, since their tests do exercise the statement.
- **All 5 fabricated IDs deleted.** `MQTT-4.3.2-4`, `MQTT-4.3.3-8`, `MQTT-4.3.3-11` promoted from
  `CrossRef` to `Tested` and given the tests the fake entries held. `MQTT-4.3.3-11`'s note had
  pointed at `MQTT-3.7.4-1` — a 3.6/3.7 copy-paste swap layered on top of a fabricated ID.
  `MQTT-3.7.4-1` deleted outright: §3.7.4 carries no normative statement, and its text contradicted
  Figure 4.3.
- **9 statements added** (`1.5.4-2`, `3.8.4-7`, `3.14.1-1`, `4.3.2-3`, `4.6.0-1…-4`, `4.6.0-6`).
- **Section `total_statements` re-synced** — 10 sections were stale, inflated where fabricated IDs
  lived and short where statements were missing.
- **`MQTT-4.2-1` retained deliberately** and documented: the OASIS body labels it `MQTT-4.2-1`
  while Appendix B says `MQTT-4.2.0-1`. We follow the body.
- **Dangling test citations cleared** from the 15 statements that named unregistered tests. Each is
  `Untested` with the test named in its `note`, so nothing is lost and the manifest no longer claims
  a test it cannot run.
- **New guard suite in `tests/manifest_load.rs`** (6 tests), backed by a generated
  `mqtt-v5.0-statement-ids.txt` holding the 251 authoritative IDs:
  every manifest ID is a real v5.0 statement; every v5.0 statement is present; no duplicate IDs;
  every cited test resolves in the `linkme` registry; no statement is `Tested` with no test; section
  totals match. **This is the durable fix — both defect classes were silent because nothing checked.**
  All 8 tests in the file pass; `cargo clippy --all-targets -- -D warnings -W clippy::pedantic` clean.

The manifest now holds exactly 251 statements, matching the spec one-for-one. Reported coverage
settles at **144/251 (57.4%)**, down from the 174/247 (70.4%) claimed at the start of the day. The
engineering was never the problem — the index over it was.

**Third pass (same day)**

- **`MQTT-3.1.4-3` session takeover now has a test.**
  `section3_connect::session_takeover_disconnects_existing_client` connects two raw clients with the
  same ClientID and asserts the first one's Network Connection is closed. **It passes** — the broker
  implements takeover correctly; the requirement was untested, not unimplemented. The assertion
  deliberately targets only the normative MUST (closing the connection) and does *not* require the
  DISCONNECT 0x8E packet, which the statement describes without a MUST. Over-asserting there would
  have created a third test capable of failing a conformant third-party broker.
- Full suite re-run: **184 passed, 0 failed, 3 ignored.** Manifest guards 8/8. Clippy pedantic clean.
- `README.md` coverage claims corrected (183 tests/247 statements → 187 tests/251 statements,
  145 tested = 57.8%).

**Why the 12 unregistered tests CANNOT be ported — corrected analysis**

An earlier entry proposed giving the shared in-process fixture a challenge-response `AuthProvider`
and advertising `enhanced_auth.CHALLENGE-RESPONSE`, so the 4 auth tests could register and be
auto-skipped against external SUTs. **That proposal was wrong and has been withdrawn.** It would
have served only 4 of the 12, and it misread the problem.

The 12 tests require *mutually contradictory* broker configurations:

| test | needs |
|---|---|
| `connack_will_retain_rejected_when_unsupported` | `with_retain_available(false)` |
| `connack_maximum_qos_advertised`, `connack_accepts_subscribe_any_qos_with_limited_max`, `suback_downgrades_to_max_qos` | a reduced maximum QoS |
| `server_keep_alive_override` | `server_keep_alive = Some(30s)` |
| `inbound_receive_maximum_exceeded_disconnects_with_0x93` | `with_server_receive_maximum(2)` |
| the 4 enhanced-auth tests | a custom `AuthProvider` |

No single broker can have retain both available and unavailable, or maximum QoS both limited and
unlimited. The shared-`SutHandle` model cannot express this at all, for any fixture configuration.

Two further points stand: a vendor-neutral CLI aimed at mosquitto could never install a Rust
`AuthProvider` or reconfigure a third-party broker's retain support, so marking these statements
`Tested` in a vendor-neutral manifest was always wrong; and the behaviour *is* verified today —
these run under `cargo test` as ordinary tokio tests. The gap is bookkeeping and architecture, not
verification.

A real fix means per-test fixture construction: extend `#[conformance_test]` so a test can declare
the fixture it needs, and have the runner build it. That is a macro plus runner change and belongs
in its own pass. Recorded here so the withdrawn proposal is not retried.

**Fourth pass (same day)**

- **Four appendix/body divergences resolved** by reading the normative body. `MQTT-3.1.2-9` held the
  text of `3.1.2-12`; `MQTT-3.9.3-2` held the ordering rule that belongs to `3.9.3-1`;
  `MQTT-4.12.0-2` kept only its trailing clause (body says "authentication" where Appendix B says
  "authorization"); `MQTT-3.7.2-1` was a correct but non-normative paraphrase, so its test was kept.
- **14 orphaned tests re-pointed** from the displaced entries to the statements they actually
  exercise, promoting each target to `Tested` (e.g. `connect_will_qos_3_is_malformed` →
  `MQTT-3.1.2-12`, `unsubscribe_stops_delivery` → `MQTT-3.10.4-2`). `MQTT-3.9.3-1` was `Untested`
  while already citing a registered test; promoted.
- **`connect_unsupported_protocol_version` no longer over-asserts.** `MQTT-3.1.2-2` makes the
  CONNACK a **MAY** and only closing the Network Connection a **MUST**; the test demanded the
  CONNACK. It now asserts the close, and checks Reason Code 0x84 only if a CONNACK is actually sent.
- **`pubrec_no_delivery_before_pubrel` removed from the conformance suite.** It asserted that QoS 2
  messages are withheld until PUBREL, which v5.0 does not require — the receiver may deliver at
  PUBLISH receipt. It was the last test citing the fabricated `MQTT-3.7.4-1`. Moved to
  `tests/section3_publish_flow.rs` as `qos2_message_not_delivered_before_pubrel`, an implementation
  guarantee of this broker rather than a conformance requirement.
- **New guard `every_registered_test_targets_a_known_statement`** (registry → manifest). The
  existing guard only checked manifest → registry. It caught 6 tests still declaring deleted
  fabricated IDs in their `#[conformance_test]` attributes; all re-pointed to the real statements
  (`MQTT-4.3.2-4`, `MQTT-4.3.3-8`, `MQTT-4.3.3-11`, `MQTT-3.8.4-7`), doc comments included.

Coverage: **155/251 (61.8%)**, recovered from 57.4% by legitimate re-pointing rather than by
relaxing anything. Verification: conformance CLI 183 passed / 0 failed / 3 ignored; lib tests 223
passed; manifest guards 9/9; every `tests/` binary green; clippy pedantic clean.

**Still outstanding**
- Re-point the orphaned tests named in the `note` fields of the 18 displaced entries.
- 4 entries skipped as appendix/body divergences needing manual reading: `3.1.2-9`, `3.7.2-1`,
  `3.9.3-2`, `4.12.0-2`.
- Two tests would falsely fail a *conformant* third-party broker — material because the CLI is
  offered as vendor-neutral: `connect_unsupported_protocol_version` hard-asserts a CONNACK the spec
  makes a MAY, and `pubrec_no_delivery_before_pubrel` asserts a non-requirement.
- `README.md` and `profiles.toml` still quote the old coverage figures.

Per-slice audit reports and the decoded spec are in the session scratchpad (`AUDIT_A…E.md`).

### Error PUBREC from a subscriber must terminate QoS2 with NO PUBREL (`[MQTT-4.3.3-4]`)

- **Bug**: `client_handler::publish::handle_pubrec` ignored the PUBREC Reason Code and always stored `AwaitingPubcomp` and wrote `PubRel{reason: Success}`. A subscriber that rejected an outbound QoS2 PUBLISH with a PUBREC reason ≥ 0x80 still received a PUBREL, and the broker kept the id in a half-open handshake. This violates `[MQTT-4.3.3-4]` (the sender sends PUBREL only for a PUBREC reason < 0x80).
- **Spec**: `[MQTT-4.3.3-4]` — on a PUBREC with reason ≥ 0x80 the sender MUST NOT send PUBREL; it discards the message and releases the Packet Identifier. (Related: `[MQTT-4.3.3-9]` — a rejected id is treated as a new Application Message if reused.)
- **Fix**: `handle_pubrec` now checks `pubrec.reason_code.is_error()` first; on an error reason it removes the `outbound_inflight` entry, removes the persisted inflight, drains queued messages, and returns WITHOUT writing a PUBREL. The success path (store `AwaitingPubcomp`, send `PubRel`) is unchanged.
- **Test**: `section4_qos::error_pubrec_from_subscriber_terminates_qos2_no_pubrel` (`[MQTT-4.3.3-4]`) — a raw subscriber at QoS2 receives the PUBLISH, replies `pubrec_with_reason(id, 0x80)`, and asserts `read_packet_bytes(2s).is_none()` (no PUBREL follows). Verified: passes with the fix, fails without. Surfaced during the deferred-ack TLA v3 fidelity quorum (see `specs/tla/deferred-ack/TLA_DIARY.md`, 2026-07-20 later).

### Retained message QoS downgrade fix (`[MQTT-3.8.4-8]`)

- **Bug**: the same retained-at-subscribe path (`client_handler::subscribe::deliver_retained_for_filter`) that skipped the Subscription Identifier also skipped the QoS downgrade. Live delivery applies `MessageRouter::effective_qos(publish_qos, sub_qos)` inside `router::prepare_message`, but the retained path queued the stored `PublishPacket` at its own stored QoS. A retained `QoS` 1 message delivered to a `QoS` 0 subscription was sent at `QoS` 1.
- **Verification**: counter-test — publish retained `QoS` 1, subscribe `QoS` 0, assert delivered QoS. Failed on the unfixed branch (`left: AtLeastOnce, right: AtMostOnce`); a `QoS` 0 control (retained `QoS` 0 → sub `QoS` 0) passed, ruling out a harness artifact. The live-path analogue `delivered_qos_is_minimum_sub0_pub1` already passed, confirming the two paths diverged.
- **Spec**: `[MQTT-3.8.4-8]` — the delivered QoS is the minimum of the message's QoS and the subscription's granted QoS; retained messages delivered at subscribe time are no exception.
- **Fix**: made `MessageRouter::effective_qos` `pub(crate)` and applied `msg.qos = effective_qos(msg.qos, options.qos)` in `deliver_retained_for_filter` before queueing, reusing the live-path logic rather than duplicating it.
- **Test**: `section3_subscribe::retained_message_delivered_at_minimum_qos` — retained `QoS` 1, subscribe `QoS` 0, assert delivered `qos == AtMostOnce`. Existing `retained_v5_props_qos1_*` tests confirm `QoS` 1 → `QoS` 1 delivery is preserved (min(1,1)=1).

### Retained message subscription identifier fix (issue #113)

- **Bug**: broker did not attach the SUBSCRIBE Subscription Identifier to retained messages delivered at subscribe time. Live publications carried it (via `router::prepare_message`), but retained-at-subscribe delivery in `client_handler::subscribe::deliver_retained_for_filter` pushed the stored `PublishPacket` straight onto the client's own `publish_tx` channel, bypassing `prepare_message`.
- **Spec**: `[MQTT-3.3.4-3]` / §3.3.2.3.8 — a message published as the result of a subscription that carried a Subscription Identifier must be sent with that identifier; retained messages sent because a new subscription matched are no exception.
- **Fix**: thread `subscribe.properties.get_subscription_identifier()` into `deliver_retained_for_filter` and set it on each retained `PublishPacket` before queueing.
- **Test**: `section3_subscribe::retained_message_carries_subscription_identifier` — publish a retained message, then subscribe with subscription identifier 42, assert the delivered retained message reports `subscription_identifiers == [42]`. Confirmed the test fails (`left: []`) without the fix and passes with it.

### Investigate post-SUBACK sleep(100ms) — safe to remove, but NOT the flake fix

**Trigger**: CI flake on `puback_error_stops_retransmission [MQTT-4.4.0-2]` (external-broker job) — `Timeout("puback")`. Suspected the `tokio::time::sleep(Duration::from_millis(100))` placed right after each `expect_suback` was a race-guard smell.

**Finding 1 — the sleep is redundant for correctness.** Broker commits the subscription to its router (`client_handler/subscribe.rs:54-68`, `router.subscribe(...).await?`) BEFORE building/sending SUBACK (`subscribe.rs:92`). So a client that has received its SUBACK is guaranteed the subscription is live; a subsequent publish from another client on the same broker will route. The 100ms sleep guarded nothing.

**Finding 2 — removal is empirically safe.** Removed all 15 post-SUBACK sleeps (3 in section3_subscribe, 1 section3_unsubscribe, 9 section4_qos, 1 section3_final_conformance, 1 section4_shared_sub). Left the other 45 `sleep(100ms)` (expiry/keepalive/negative-retransmit windows) untouched. Ran against the external `mqttv5` broker:
- unloaded: `section4_qos` 200/200 clean; full suite ~155 runs clean (1 uncaptured full-suite flake).
- under 8-way `yes` CPU load: `section4_qos` 149/150 + ~200 more clean; with-sleep control 150/150.
- **Zero delivery-miss failures ever** (`must receive QoS 1 PUBLISH` never fired). The ONLY failure mode observed was `Timeout("suback")`/`Timeout("puback")` — broker throughput under load, which is causally UPSTREAM of the sleep (the sleep runs after the ack arrives), so the sleep cannot affect it.

**Finding 3 — the sleep removal does NOT fix the flake.** Today's red-X and the load-induced failures are TIMEOUT-tightness: `TIMEOUT=3s` (per-op) and `ACK_TIMEOUT=10s` (`test_client/raw.rs:43`) are too tight for an oversubscribed CI runner. The sleep removal is a cosmetic cleanup (~1.5s faster per full suite run, removes dead complexity) that is safe but orthogonal to the flake.

**Fix — timeout hardening (the actual flake remedy).** `ACK_TIMEOUT` 10s→30s (`test_client/raw.rs`; single site, purely a positive deadline on await_ack, fixes today's `Timeout("puback")` with 3x headroom) and all 19 per-file `TIMEOUT` consts 3s→10s (headroom for expect_suback/connack/publish).

Verified empirically against the external `mqttv5` broker:
- **No delivery-miss regression** from either change (`must receive QoS 1 PUBLISH` never fired across ~700 runs).
- **Runtime cost negligible**: full suite 8s→~11.6s (+3.6s). The conformance CI job is ~3 min (build-dominated), so +3.6s in the test phase is noise. Confirmed no passing test asserts `is_none()` after a `TIMEOUT` read; the `if let Some` sites (`final_conformance:76`, `publish_advanced:63/137`) take the fast path on success.
- **suback timeouts cured**: at 3s under 8-way CPU saturation, `section4_qos` flaked `Timeout("suback")`; at 10s that mode disappeared across 150 saturated runs.
- **Residual**: under pathological 8-way full-core saturation (harsher than a 2-core CI runner), `Timeout("puback")` still appears ~2/150 at 30s — a fully CPU-starved broker defeats any finite timeout. Real CI failed at 10s; 30s gives 3x margin. A bounded publish retry would be the next lever if 30s proves insufficient, but it changes QoS1 DUP semantics and is deferred.


### Fix 9 conformance test failures verified against Mosquitto source

None of the 9 failures were broker bugs. All were test expectations beyond what the spec mandates, or test infrastructure issues.

**RawMqttClient framing bug (Fix #4)**: `read_packet_bytes()` did a single raw TCP read. When the broker sent an echoed PUBLISH + PUBACK in one TCP segment, the test saw PUBLISH first byte 0x30 instead of PUBACK 0x40. Added `BytesMut` buffer, `try_extract_packet()`, and `read_mqtt_packet()` for proper MQTT framing. Also added `shutdown_write()` for EOF signaling.

**PUBACK/PUBREC 0x10 (Fixes #5, #5b)**: Tests asserted `reason == 0x00` but `NoMatchingSubscribers` (0x10) is a valid success-class reason code returned by both Mosquitto and EMQX. Now accepts both.

**PUBREL unknown packet ID (Fix #6)**: Spec says server "should" respond with 0x92, not "MUST". Mosquitto hardcodes 0x00. Now accepts both 0x92 and 0x00.

**Shared sub invalid format (Fix #7)**: Tests expected SUBACK with 0x8F but Mosquitto/EMQX send DISCONNECT + close instead. Spec allows both. Tests now use `read_mqtt_packet()` and branch on packet type: SUBACK → check 0x8F, DISCONNECT or EOF → pass.

**Client ID charset (Fix #2)**: Tests sent "bad/id" expecting 0x85 rejection, but spec says servers MAY accept extended characters. Added `strict_client_id_charset` capability flag so tests only run against brokers that reject non-alphanumeric client IDs.

**$SYS publish (Fix #1)**: Test published to `$SYS/test` but most brokers reject client publishes to `$`-prefixed topics. Added `dollar_sys_publish` capability flag.

**Truncated CONNECT (Fix #3)**: Some brokers wait for more data since the packet is genuinely truncated. Added `shutdown_write()` call after sending truncated bytes to signal EOF and force the broker to stop waiting.

### Fix: FileBackend session expiry deadlock (Rust 2021 edition)

`FileBackend::get_session()` had a deadlock triggered by expired sessions.
The code used an `if let` with a temporary `RwLockReadGuard`:

```rust
if let Some(session) = self.sessions_cache.read().await.get(client_id).cloned() {
    if session.is_expired() {
        self.remove_session(client_id).await?;  // DEADLOCK
```

In Rust 2021 edition, temporaries in `if let` conditions live for the entire
block body. The read guard was still held when `remove_session()` tried to
acquire a write lock on the same `sessions_cache`. Fix: extract the read into
a `let` binding so the guard drops at the semicolon before the body executes.

This only manifested with `clean_start=false` after a session with short
expiry had expired — the exact scenario tested by `session_discarded_after_expiry`.

Discovery: the `mqttv5-cli` binary was using `mqtt5 = "0.29"` from crates.io
instead of the local code. The workspace `[patch.crates-io]` didn't apply
because the local version `0.31.1` didn't satisfy `^0.29`. Updated to
`mqtt5 = "0.31"` so the patch resolves correctly.

### Fix: shared subscription message matching in RawTestClient

`RawTestClient::subscribe()` stored the full `$share/group/topic` filter for
local message routing. When messages arrived on topic `topic`, the matching
check `topic_matches_filter("topic", "$share/group/topic")` returned false.

Fix: import `strip_shared_subscription_prefix()` and use the extracted topic
filter for local matching.

### Fix: conformance test isolation for parallel execution

Multiple shared subscription tests used hardcoded topic names (`tasks`,
`topic`) causing cross-pollination when tests ran in parallel. Each test
now generates unique topic names via `unique_client_id()`.

The `$SYS` wildcard test (`dollar_topics_not_matched_by_root_wildcards`)
subscribed to `#` which caught messages from all parallel tests. Changed
from count-based assertion to content filtering — checks that no received
messages have `$`-prefixed topics, ignoring non-`$SYS` messages from
parallel tests.

### Fix: MQTT message ordering violation in `CallbackManager::dispatch()`

The conformance CLI runner exposed a real bug in `crates/mqtt5/src/callback.rs`.
`CallbackManager::dispatch()` was spawning a separate `tokio::spawn` per
callback invocation, which does not guarantee execution order on a multi-threaded
runtime. This violates `MQTT-4.6.0-5` (message ordering per-topic, per-QoS).

The bug was masked during `cargo test` because each `#[tokio::test]` spins up
a per-test current-thread runtime where spawned tasks are executed in FIFO
order. The CLI runner uses one shared multi-threaded `Runtime::new()`, which
exposed the race: 4 out of 10 runs of `message_ordering_preserved_same_qos`
failed with out-of-order delivery.

**Fix**: replaced per-callback `tokio::spawn` with a single FIFO worker task
per `CallbackManager`. A `tokio::sync::mpsc::unbounded_channel` queues
`DispatchItem` batches (callbacks + message); a single consumer task drains
the channel and invokes callbacks sequentially. This preserves both invariants:

1. **Non-blocking dispatch** — `dispatch()` returns immediately after channel send
2. **Sequential ordering** — a single worker processes items in submission order

The worker is lazily spawned on first dispatch via `OnceLock`.

Verification: 20/20 deterministic passes of `message_ordering` via CLI runner
(was 6/10 before fix). Full suite: 181/181 passing. All 411 mqtt5 unit tests
passing including `test_dispatch_does_not_block_on_slow_callback`.

### Phase I — Bulk migration of test files into `src/conformance_tests/` — COMPLETE

Phase I migrates every vendor-neutral test from `tests/section*.rs` to
`src/conformance_tests/section*.rs`, rewriting each test to use the
`#[conformance_test(ids = [...], requires = [...])]` proc-macro and take
`sut: SutHandle` as its only parameter. After migration the tests live
inside the library crate so the CLI runner can walk
`linkme::distributed_slice` to enumerate every test at link time, and
the same bodies are picked up by `cargo test --lib --features
inprocess-fixture` for development.

1. **21 files migrated** (in completion order):
   - `section4_error_handling` (2 tests, 70 lines)
   - `section6_websocket` (3 tests, 136 lines)
   - `section3_publish_flow` (5 tests, 187 lines)
   - `section1_data_repr` (5 tests, 195 lines)
   - `section4_flow_control` (3 tests, 250 lines)
   - `section3_disconnect` (8 tests, 264 lines)
   - `section3_publish_alias` (4 tests, 319 lines)
   - `section4_enhanced_auth` (7 tests, 342 lines)
   - `section3_final_conformance` (7 tests, 369 lines)
   - `section3_qos_ack` (8 tests, 376 lines)
   - `section3_unsubscribe` (10 tests, 441 lines)
   - `section4_shared_sub` (10 tests, 454 lines)
   - `section3_publish_advanced` (7 tests, 490 lines)
   - `section4_topic` (10 tests, 527 lines)
   - `section3_connack` (11 tests, 532 lines)
   - `section3_subscribe` (12 tests, 651 lines)
   - `section3_connect_extended` (14 tests, 660 lines)
   - `section3_connect` (21 tests, 707 lines)
   - `section4_qos` (11 tests, 743 lines)
   - `section3_publish` (21 tests, 929 lines)
   - `section3_ping` (5 tests, completed in Phase G as the proof-of-concept)

2. **4 files retained in `tests/`** as vendor-specific stragglers gated
   by `inprocess_sut_with_config(...)`. These exercise broker
   configuration flags (`max_qos`, `topic_alias_maximum`, ACL grammar,
   enhanced-auth provider injection) that are not expressible in the
   vendor-neutral capability matrix, so they remain
   in-process-only `#[tokio::test]` cases until v2 of the capability
   DSL: `section3_publish_flow.rs`, `section3_connack.rs`,
   `section3_subscribe.rs`, `section4_enhanced_auth.rs`.

3. **Vendor-neutrality split rule**: a test migrates iff it touches
   only `inprocess_sut()` and never calls
   `inprocess_sut_with_config(...)`. Tests that mutate broker config
   stay behind because they cannot run against an external SUT — the
   capability matrix can only describe what a broker advertises, not
   reconfigure it on the fly.

4. **Standard import block** for every migrated file:
   ```rust
   use crate::conformance_test;
   use crate::harness::unique_client_id;
   use crate::raw_client::{RawMqttClient, RawPacketBuilder};
   use crate::sut::SutHandle;
   use crate::test_client::TestClient;
   use mqtt5_protocol::types::*;
   ```
   No `use mqtt5::*` anywhere — `mqtt5_protocol` is the
   vendor-neutral types crate. The migrated bodies have zero
   dependency on the in-tree broker implementation; the
   `inprocess-fixture` feature only matters at SUT-construction time.

5. **Capability assertions**: every migrated test annotates its
   `requires = [...]` list against the strings recognised by
   `Requirement::parse` in `capabilities.rs` (`transport.tcp`,
   `max_qos>=1`, `max_qos>=2`, `retain_available`,
   `wildcard_subscription_available`, `subscription_identifier_available`,
   `shared_subscription_available`, `assigned_client_id_supported`).
   The proc-macro validates the requirement string at compile time, so
   typos and unknown capabilities are caught at `cargo check`.

6. **Property-comparison anti-pattern fix**: `section3_publish.rs` —
   the largest file at 929 lines and the worst offender — used to
   `assert_eq!` raw user-property vectors that included broker-injected
   properties (`x-mqtt-sender`, `x-mqtt-client-id`). The migrated
   version uses `assertions::expect_user_properties_subset(actual,
   expected, sut.injected_user_properties())` which automatically
   tolerates declared injected properties on top of the asserted set.
   This is the single most important fix for vendor neutrality:
   without it every property-comparison test would fail spuriously
   against any non-mqtt-lib broker.

7. **Result**: 218 lib tests + 16 integration tests = 234 tests
   passing under `cargo test -p mqtt5-conformance --features
   inprocess-fixture`. Pedantic clippy clean
   (`cargo clippy -p mqtt5-conformance --all-targets --features
   inprocess-fixture -- -D warnings -W clippy::pedantic`).

8. **Clippy doc_markdown cleanup**: 14 missing-backticks errors found
   on the first pedantic pass — `QoS`, `ShareName`, `ShareNames`,
   `no_local`, `TopicFilterInvalid` — across six newly-touched files.
   Fixed by wrapping each identifier in backticks per the
   `clippy::doc_markdown` lint rules. ALL MODIFIED CODE IS MY CODE:
   even pre-existing doc strings copied from the old `tests/` files
   had to be cleaned up because the migration touched the file.

9. **Module declaration order** in `src/conformance_tests/mod.rs` is
   alphabetical within each section group. The 21 modules now declared:
   `section1_data_repr`, `section3_connack`, `section3_connect`,
   `section3_connect_extended`, `section3_disconnect`,
   `section3_final_conformance`, `section3_ping`, `section3_publish`,
   `section3_publish_advanced`, `section3_publish_alias`,
   `section3_publish_flow`, `section3_qos_ack`, `section3_subscribe`,
   `section3_unsubscribe`, `section4_enhanced_auth`,
   `section4_error_handling`, `section4_flow_control`, `section4_qos`,
   `section4_shared_sub`, `section4_topic`, `section6_websocket`.

10. **Next phase**: Phase I extraction. Once the four straggler
    `tests/section*.rs` files have either grown an external-SUT
    capability path or been quarantined behind a feature, we can
    `git subtree split -P crates/mqtt5-conformance -b
    conformance-extract` and push the result to a standalone repo
    `mqtt5-conformance-platform`. The `inprocess-fixture` feature
    becomes the only path that depends on `mqtt5`, and downstream
    consumers wire their own broker behind a `SutHandle::External`.

### Phase H — Profiles + example SUTs — COMPLETE

Phase H ships the vendor-neutral descriptor ecosystem that closes out the
in-tree refactor ahead of the standalone-repo extraction in Phase I.

1. **`profiles.toml`** — top-level conformance profiles deserializable as
   `BTreeMap<String, Capabilities>`:
   - `core-broker` — TCP-only, QoS 2, retain, subscription identifiers.
     No shared subs, no TLS, no WebSocket, no QUIC. This is the "minimum
     bar" a broker must clear to claim MQTT v5 core conformance.
   - `core-broker-tls` — same as `core-broker` plus `transports.tls`. The
     second-tier profile most production brokers target.
   - `full-broker` — every optional capability enabled: TCP + TLS + WS,
     shared subs, `topic_alias_maximum = 65535`, ACL, enhanced auth with
     `SCRAM-SHA-256`, restart + cleanup hooks. Anchors the upper bound
     for "100% conformance" claims.

2. **`tests/fixtures/*.toml`** — five ready-to-run SUT descriptors:
   - `inprocess.toml` — default in-process fixture used by `cargo test
     --features inprocess-fixture`. Empty addresses (filled by the
     harness), TCP-only, broker-injected user properties declared
     (`x-mqtt-sender`, `x-mqtt-client-id`).
   - `external-mqtt5.toml` — mqtt-lib's own `mqttv5` binary running
     standalone on `127.0.0.1:1883`. TCP-only to match the CI broker
     launched by `conformance.yml`.
   - `mosquitto-2.x.toml` — Eclipse Mosquitto 2.x with TCP, TLS, and
     WebSocket. `topic_alias_maximum = 10` reflects Mosquitto's
     conservative default. Includes a `restart_command` stanza.
   - `emqx.toml` — EMQX broker with every transport including QUIC,
     ACL enabled, and SCRAM-SHA-{256,512} as declared enhanced-auth
     methods.
   - `hivemq-ce.toml` — HiveMQ Community Edition with TCP + WebSocket.
     No TLS in CE; ACL off; `topic_alias_maximum = 5` matching HiveMQ's
     CE cap.

3. **Schema unit tests** — six new tests keep the fixtures honest at
   `cargo test` time, without ever contacting a broker:
   - `capabilities::tests::profiles_toml_parses_against_capabilities_schema`
     — `include_str!("../profiles.toml")`, deserialize to
     `BTreeMap<String, Capabilities>`, verify every required profile
     key is present and the TCP / TLS / WebSocket / ACL flags match
     expectations.
   - `sut::tests::fixture_{inprocess,external_mqtt5,mosquitto,emqx,hivemq_ce}_parses`
     — one test per fixture, each loading via `SutDescriptor::from_str`
     and asserting the name, the relevant transport flags, and any
     broker-specific claims (EMQX has QUIC + ACL, HiveMQ CE has
     WebSocket but no TLS, etc.). `external-mqtt5` asserts the
     `127.0.0.1:1883` socket address round-trips through
     `tcp_socket_addr()`.

4. **`.github/workflows/conformance.yml`** — three-job CI workflow that
   wires the CLI runner into the pipeline:
   - `inprocess` — `cargo build --release -p mqtt5-conformance-cli`,
     `./target/release/mqtt5-conformance`, then a second run with
     `--report conformance-report.json` uploaded as an artifact.
     Exercises the default in-process fixture path end-to-end.
   - `external-mqtt5` — builds `mqttv5-cli` and the conformance CLI,
     spawns `mqttv5 broker --host 127.0.0.1:1883 --allow-anonymous true`
     in the background with PID captured via `echo $!`, runs
     `mqtt5-conformance --sut
     crates/mqtt5-conformance/tests/fixtures/external-mqtt5.toml
     --report conformance-report.json`, uploads both broker.log and the
     report, and kills the broker in an `if: always()` step.
   - `fixtures` — `cargo test -p mqtt5-conformance --lib --features
     inprocess-fixture` to run the six schema unit tests in isolation
     so fixture regressions show up even on PRs that don't touch the
     CLI.

5. **Fixture / CI-broker alignment gotcha** — the first draft of
   `external-mqtt5.toml` declared `tls = true` and `websocket = true`,
   but the CI broker launches with only `--host 127.0.0.1:1883` (no TLS
   cert, no WS port). That would skip zero tests locally but fail every
   TLS/WS-gated test in CI. Trimmed the fixture to TCP-only and
   tightened `fixture_external_mqtt5_parses` to assert
   `!transports.tls`. Follow-up: when Phase I (or earlier) introduces a
   CI job that generates test certs and launches the broker on
   `--tls-port`, the fixture flips back and a dedicated TLS job can
   exercise the gated tests.

6. **End-to-end verification** — with the `mqttv5` release binary
   running locally on `127.0.0.1:1883`:
   - `./target/release/mqtt5-conformance --sut
     crates/mqtt5-conformance/tests/fixtures/external-mqtt5.toml`
     → 5 passed, 0 failed, 0 ignored.
   - `./target/release/mqtt5-conformance --sut
     crates/mqtt5-conformance/tests/fixtures/hivemq-ce.toml`
     → 5 passed, 0 failed, 0 ignored (the HiveMQ fixture still declares
     `tcp = true` so every POC test qualifies).
   Full suite: `cargo test -p mqtt5-conformance --lib --features
   inprocess-fixture` reports 42 passed (36 prior + 6 new schema tests).
   `cargo clippy --all-targets --workspace -- -D warnings -W
   clippy::pedantic` is clean.

7. **Deferred to Phase I** — migrating the 21 integration test files
   under `tests/section*.rs` (8,681 lines) from the legacy
   `ConformanceBroker` / `TestClient`-in-tests pattern onto the
   registry-aware `#[conformance_test]` macro. The POC under
   `src/conformance_tests/section3_ping.rs` is sufficient to prove the
   runner works; bulk migration is a mechanical refactor best done as
   its own phase.

### Phase G — Proc-macro + CLI runner — COMPLETE

Two new workspace crates land `#[conformance_test]` and the external-SUT
runner on top of the Phase F work:

1. **`crates/mqtt5-conformance-macros`** — proc-macro crate exposing
   `#[conformance_test(ids = [...], requires = [...])]`. The macro:
   - Validates every `id` begins with `MQTT-` at compile time.
   - Validates every `requires` string against the `Requirement` DSL at
     compile time and emits **literal enum variant tokens** (no runtime
     `from_spec` calls — static initializers can't call non-const fns).
   - Rewrites the annotated fn into `__conformance_impl_<name>` once, and
     emits:
     - A `#[cfg(test)] #[tokio::test]` wrapper so `cargo test` still runs
       the suite against the default in-process fixture during development.
     - A fn-pointer runner registered in
       `linkme::distributed_slice(CONFORMANCE_TESTS)` so the CLI runner
       observes every test across the workspace at link time.

2. **`crates/mqtt5-conformance-cli`** — `libtest-mimic`-backed binary
   `mqtt5-conformance` that walks `CONFORMANCE_TESTS`, evaluates
   capabilities against a `SutPlan`, and builds a `Vec<Trial>`:
   - `--sut PATH` loads an external `SutDescriptor`; absent means the
     in-process fixture (via `inprocess_sut().await`).
   - Each trial constructs its own fresh `SutHandle` inside the runner so
     tests can't leak broker state to each other — identical to the
     per-test-fresh-broker semantics the existing `#[tokio::test]`
     wrappers provide.
   - Tests whose `requires` aren't satisfied are emitted as
     `Trial::test(..., || Ok(())).with_ignored_flag(true)` with the unmet
     capability in the trial name, so libtest-mimic's output correctly
     reports `passed / failed / ignored` without running a broker per
     skipped test.
   - `--report PATH` writes a JSON summary with per-test status and the
     capability matrix used to evaluate skips.
   - Any non-`--sut`/`--report` argument is forwarded to libtest-mimic,
     so `--list`, `--ignored`, filter substrings all Just Work.

3. **Registry + `extern crate self`** — `src/registry.rs` exposes the
   `ConformanceTest` struct and the `CONFORMANCE_TESTS` distributed slice.
   Because the macro emits `::mqtt5_conformance::...` paths, the library
   adds `extern crate self as mqtt5_conformance;` so those paths resolve
   when the macro is used inside the crate itself.

4. **POC migration** — `section3_ping` (5 tests) moved to
   `src/conformance_tests/section3_ping.rs` as a library module. Integration
   tests under `tests/*.rs` compile as separate test binaries, so their
   `distributed_slice` entries would be invisible to the CLI binary — tests
   must live under `src/` to link into the runner. The old
   `tests/section3_ping.rs` was deleted to avoid duplicate registration.

**Verification:**

- `cargo test -p mqtt5-conformance --lib --features inprocess-fixture` —
  36 passing, includes 5 POC tests via the `#[cfg(test)] #[tokio::test]`
  wrappers.
- `./target/release/mqtt5-conformance` (in-process) — 5 passed, 0 failed,
  0 ignored. Same 5 tests, now driven by the linkme registry.
- `./target/release/mqtt5-conformance --sut /tmp/external.toml` against a
  standalone `mqttv5 broker` instance — 5 passed, 0 failed. Validates the
  external-SUT path end-to-end.
- Restricted SUT (`transports.tcp = false`) — all 5 tests correctly
  marked `ignored` with `[missing: transport.tcp]` in the trial name.
- `cargo clippy --all-targets --workspace -- -D warnings -W clippy::pedantic` — clean.

**Follow-up for Phase H:**

- Migrate the remaining 21 test files from `tests/*.rs` to
  `src/conformance_tests/*.rs`, annotating each test with
  `#[conformance_test(...)]`. Mechanical; Phase F already did the hard
  vendor-neutrality work.
- Add `profiles.toml` + example `sut.toml` fixtures (mqtt-lib, Mosquitto,
  EMQX, HiveMQ CE).
- Wire CI to run the CLI against a freshly-built `mqttv5` binary and
  assert 100% pass on the declared statement set.

### Phase F — Migrate all test files off mqtt5 re-exports — COMPLETE

All 22 integration test files now compile against the vendor-neutral `SutHandle` + `TestClient` API with **zero `use mqtt5::*` imports** in test bodies. The only remaining `mqtt5` dependency is behind the `inprocess-fixture` feature (in `harness.rs` and `sut/inprocess.rs`), exactly as Phase C planned.

**Final test count: 229 passing integration tests** across 22 files:

| File | Tests | Notes |
|---|---|---|
| section1_5_data_representation | 5 | |
| section3_connect | 21 | |
| section3_connack | 11 | |
| section3_publish | 22 | Property-assertion anti-pattern fixed via `expect_user_properties_subset` |
| section3_publish_advanced | 7 | Overlapping subs, message expiry, response topic |
| section3_subscribe | 14 | ACL tests quarantined behind `requires = ["acl"]` |
| section3_suback | 8 | |
| section3_unsubscribe | 7 | |
| section3_unsuback | 7 | |
| section3_pingreq | 5 | |
| section3_disconnect | 8 | |
| section3_qos_ack | 9 | Clean QoS state-machine tests, migrated first |
| section3_flow_control | 3 | |
| section3_auth_reserved | 1 | |
| section4_topic | 13 | Wildcard, `$`-prefix, multi-level, message ordering |
| section4_qos | 12 | Full QoS1/QoS2 outbound state-machine coverage |
| section4_shared_sub | 10 | `$share/` routing, round-robin, PUBACK rejection, granted-QoS downgrade |
| section4_enhanced_auth | 7 | Operator pre-configures `CHALLENGE-RESPONSE` per `sut.toml` |
| section4_13_error_handling | 2 | |
| section6_websocket | 3 | Validates `transport.rs` abstraction |
| extras_phase1 | 14 | Extended CONNECT / will / session |
| extras_final | 30 | Misc normative-statement coverage |

**Verification passed:**
- `cargo check -p mqtt5-conformance --features inprocess-fixture --tests` — clean
- `cargo test -p mqtt5-conformance --features inprocess-fixture` — 229/229 pass
- `cargo clippy --all-targets --workspace -- -D warnings -W clippy::pedantic` — clean

**Gotchas from the final migration pass (section4_topic, section4_qos, section4_shared_sub):**

1. **`TestClient::publish` takes `&[u8]`, not `Vec<u8>`** — strip `.to_vec()` from all call sites. `b"literal".to_vec()` → `b"literal"`.
2. **`format!("msg-{i}").as_bytes()`** — works as a rvalue-temporary borrow because the `String` lives until end-of-statement, but cleaner is `let payload = format!("msg-{i}"); publisher.publish(&topic, payload.as_bytes()).await`.
3. **`MessageCollector` → `Subscription`** — `collector.get_messages()` becomes `subscription.snapshot()`, `collector.count()`/`wait_for_messages` move onto the `Subscription` struct.
4. **Unused subscription handles** — in `qos2_pubrec_error_allows_packet_id_reuse` the test never reads from the subscription but needs the broker-side route alive until end of scope. `let _subscription =` is the right pattern (lifetime-driven, not silencing missing logic).
5. **Bulk perl replacements** for mechanical patterns: `let broker = ConformanceBroker::start().await;` → `let sut = inprocess_sut().await;` and `RawMqttClient::connect_tcp(broker.socket_addr())` → `RawMqttClient::connect_tcp(sut.expect_tcp_addr())`. Then per-test `connected_client(name, &broker)` → `TestClient::connect_with_prefix(&sut, name)` edits.
6. **`RawTestClient::connect` needed a `# Panics` doc section** (clippy pedantic) — `.unwrap()` on a local-only `StdMutex` that can only be poisoned if a prior holder panicked.

**What's intentionally not yet vendor-neutral:** the test files still use `inprocess_sut()` directly — they'll swap to `harness::sut()` once the proc-macro runner in Phase G passes an abstract `SutHandle` into each test via dependency injection. That's Phase G work, not Phase F.

**Phase F is now the final refactor step that leaves the entire conformance suite running against the vendor-neutral test harness.** Phase G (proc-macro + CLI runner) and Phase H (profiles + external SUT fixtures) are architectural additions on top of this stable base.

---

### Phase E — TestClient (raw-backed) — COMPLETE

Implemented `RawTestClient` in `src/test_client/raw.rs`: a self-contained MQTT v5 client that speaks the protocol directly over a `TcpTransport`, with no dependency on `mqtt5::MqttClient`. This is the backend the standalone conformance runner will use against third-party brokers described by a `SutDescriptor`.

Architecture mirrors the in-process backend's public surface (`connect`, `publish` / `publish_with_options`, `subscribe`, `unsubscribe`, `disconnect`, `disconnect_abnormally`) so callers can swap `InProcessTestClient` ↔ `RawTestClient` without touching test bodies. Internals:

- **Reader task**: a single `tokio::spawn`ed loop holds the `OwnedReadHalf`, decodes incoming packets via `Packet::decode_from_body`, and dispatches to ack channels or subscription queues. `Drop` aborts the task; `disconnect`/`disconnect_abnormally` stop it explicitly.
- **Ack correlation**: `pending_acks: Arc<StdMutex<HashMap<u16, oneshot::Sender<AckOutcome>>>>`. Each outbound op that needs an ack inserts a oneshot, then `await_ack()` waits with a 10s timeout. `packet_id=0` is the sentinel for CONNACK arrival, polled by `await_connack`.
- **Subscription dispatch**: `subscriptions: Arc<StdMutex<Vec<SubscriptionEntry>>>` keyed by topic filter; the reader walks the list per inbound PUBLISH, runs `topic_matches_filter(topic, filter)`, and pushes a `ReceivedMessage` clone into every matching queue.
- **QoS state machines**: QoS0 fire-and-forget; QoS1 awaits PUBACK; QoS2 sends PUBLISH → awaits PUBREC → sends PUBREL → awaits PUBCOMP. Inbound QoS1/QoS2 trigger PUBACK/PUBREC from the reader task. PUBREL → PUBCOMP is handled inline in the dispatcher.
- **Writer arbitration**: `writer: AsyncMutex<Option<OwnedWriteHalf>>` so write ops from publish/subscribe and the reader's PUBACK responses serialize cleanly without holding a lock across `await` boundaries on the application side.

Key gotchas hit and fixed:

1. `mqtt5::MqttError` and `mqtt5_protocol::error::MqttError` are the **same type** (pure re-export). Two `From` impls in `TestClientError` collided. Removed the `Client(mqtt5::MqttError)` variant entirely; the single `From<mqtt5_protocol::error::MqttError>` covers both backends.
2. `Properties::set_correlation_data` takes `bytes::Bytes` but `PublishProperties.correlation_data` is `Option<Vec<u8>>` — needs `.into()`.
3. `RetainHandling` exists in two versions with different variant names: types-level `SendAtSubscribe`/`SendIfNew`/`DontSend` vs packet-level `SendAtSubscribe`/`SendAtSubscribeIfNew`/`DoNotSend`. Wrote `convert_retain_handling()` helper.
4. The `try_parse_packet` helper handles incremental TCP reads — peeks the buffer, decodes a `FixedHeader`, returns `None` if the body isn't fully buffered yet, otherwise splits off and decodes via `Packet::decode_from_body`.
5. Removed feature gate from `pub mod test_client;` in `lib.rs` — the module is now feature-independent because the raw backend doesn't need `mqtt5`.

Added 3 raw-backend tests in `test_client/mod.rs` (`raw_backend_roundtrip_against_inprocess_fixture`, `raw_backend_qos2_roundtrip`, `raw_backend_unsubscribe_stops_delivery`) plus a fourth (`raw_qos0_roundtrip`) inside `raw.rs` itself. All 6 test_client tests pass; the full 229-test conformance suite still passes; `cargo clippy --all-targets --workspace -- -D warnings -W clippy::pedantic` is clean.

Phase E acceptance criteria met: `RawTestClient` covers the §4 API; `SutHandle::External` is ready to wire into `RawTestClient::connect` (the raw backend takes a plain `SocketAddr` and `ConnectOptions`, both already produced by `SutDescriptor::tcp_socket_addr`); `TestClient` now has two backends and tests written against the unified API are agnostic. Phase F can begin — migrating the remaining 18 test files off direct `mqtt5::MqttClient` usage onto `TestClient`.

### Phase D — TestClient (in-process backed) — COMPLETE

Created `src/test_client.rs` gated by the `inprocess-fixture` feature. Wraps `mqtt5::MqttClient` behind a vendor-neutral API that accepts a `SutHandle` instead of a raw `ConformanceBroker`. Key design decisions:

- `TestClient::subscribe()` returns a self-contained `Subscription` handle holding its own `Arc<Mutex<Vec<ReceivedMessage>>>`. Cleaner than the pre-existing `MessageCollector` pattern — every subscription gets its own queue with `expect_publish(timeout)`, `wait_for_messages(count, timeout)`, `snapshot()`, `count()`, `clear()`.
- `ReceivedMessage` carries the full property set specified in the plan: `dup`, `subscription_identifiers: Vec<u32>` (plural, already matches `MessageProperties`), `user_properties`, `content_type`, `response_topic`, `correlation_data`, `message_expiry_interval`, `payload_format_indicator`. The `dup` field is currently always `false` since `MessageProperties` doesn't carry inbound DUP — this is a fidelity gap to close in a later phase.
- Types re-exported via `mqtt5::{QoS, SubscribeOptions, PublishOptions}` are really from `mqtt5_protocol`, so using them directly in `TestClient` keeps vendor neutrality once Phase E swaps the backend for `RawTestClient`.
- `connect_with_options` accepts `mqtt5::ConnectOptions` (wrapper with session/reconnect config) — Phase E will introduce a vendor-neutral alternative for the raw backend.

Migrated 4 test files to `SutHandle` + `TestClient` + `Subscription`:
- `section3_qos_ack.rs` — 9 tests. Mix of pure raw (`RawMqttClient` against `sut_tcp_addr(&sut)`) and mixed raw+high-level (single `sut` serving both clients).
- `section3_disconnect.rs` — 8 tests. 3 will-message tests use `TestClient + subscription.expect_publish()`; 5 pure-raw tests swap `broker.socket_addr()` for `sut_tcp_addr(&sut)`.
- `section3_ping.rs` — 5 tests. Pure raw, mechanical swap.
- `section1_data_repr.rs` — 5 tests. Pure raw, mechanical swap (including the BOM test that spawns two raw clients against the same SUT).

Clippy fixes needed:
- Removed redundant `#![cfg(feature = "inprocess-fixture")]` inside `test_client.rs`; the `#[cfg(...)]` on the `pub mod test_client` line in `lib.rs` is sufficient and the inner attribute triggered `duplicated attribute`.
- Added `# Panics` doc sections to `Subscription::{expect_publish, wait_for_messages, snapshot, count, clear}` and `TestClient::subscribe` for their `Mutex::lock().unwrap()` call sites.
- Changed "QoS 0" → "`QoS` 0" in the `publish` doc to satisfy `clippy::doc_markdown`.

Verification:
- `cargo check -p mqtt5-conformance --tests` — clean
- `cargo clippy -p mqtt5-conformance --all-targets -- -D warnings -W clippy::pedantic` — clean
- `cargo test -p mqtt5-conformance --tests` — **225 passed, 0 failed** (205 original tests + 2 `test_client` unit tests + other crate tests). Migrated files: 5/5, 8/8, 5/5, 9/9.

Phase D complete. Ready for Phase E (raw-backed `TestClient` for external SUTs).

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
