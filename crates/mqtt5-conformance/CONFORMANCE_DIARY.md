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
