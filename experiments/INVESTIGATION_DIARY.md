# HOL Blocking Investigation Diary

New entries on top, beneath the planned work list.

## Planned Work

- [x] Phase 1: Fix DataPerSubscription = DataPerTopic alias
- [x] Phase 2: Add broker delivery strategy support (ControlOnly, PerTopic, PerPublish)
- [x] Phase 3: Tune QUIC flow control to reduce buffer bloat
- [ ] Phase 4: Re-run experiment 02 with fixes

---

## 2026-03-04 — Verification: clippy + tests pass clean

**What**: Ran full verification suite after all Phase 1-3 changes.

**Why**: Ensure no regressions from the three phases of changes (DataPerSubscription deprecation, ServerDeliveryStrategy addition, QUIC flow control tuning).

**Result**: `cargo clippy --all-targets --workspace -- -D warnings -W clippy::pedantic` passes clean. `cargo test --workspace --exclude mqtt5-conformance` passes with 0 failures across all test suites (unit tests, integration tests, property tests, doc-tests). Two minor clippy fixes applied: removed unused `strategy()` getter from `ServerStreamManager`, merged duplicate `match_same_arms` in bench_cmd.rs.

**Next**: Phase 4 — re-run experiment 02 on GCP with the fixes (requires starting VMs, updating external IPs, regen certs).

## 2026-03-03 — Phase 3: QUIC flow control tuned to reduce buffer bloat

**What**: Reduced Quinn transport config defaults for both client and server:
- `stream_receive_window`: 256 KiB (was ~1.25 MiB default)
- `receive_window`: 1 MiB (was ~8 MiB default)
- `send_window`: 1 MiB (was ~10 MiB default)

**Why**: Quinn defaults are tuned for 100 Mbps links — way too large for MQTT's small messages. With 8 per-topic streams, old defaults allowed ~10 MiB of buffered data before backpressure, causing 6-17s latency under 1% loss. New limits: 8 streams × 256 KiB = 2 MiB max before stream-level backpressure, plus 1 MiB connection-level send cap.

**Result**: Both client and server transport configs now have bounded windows. Per-topic latency under loss should improve significantly as `publish().await` will block sooner instead of buffering indefinitely.

**Next**: Re-run experiment 02 to verify improvement.

## 2026-03-03 — Phase 2: Broker delivery strategy support added

**What**: Added `ServerDeliveryStrategy` enum with three variants: `ControlOnly`, `PerTopic` (default, existing behavior), `PerPublish` (new). Wired through `BrokerConfig`, `ClientHandler`, `ServerStreamManager`, and CLI (`--quic-delivery-strategy`).

**Why**: Previously the broker ALWAYS delivered via per-topic streams regardless of the client's `--quic-stream-strategy`. Experiments measured a mix of publisher-side and subscriber-side HOL blocking effects. Now experiments can set broker delivery to `control-only` to isolate publisher-side effects, or `per-publish` for maximum stream independence on both sides.

**Result**: `ServerStreamManager` now supports `PerPublish` (ephemeral streams with finish + yield) and `ControlOnly` (falls back to control stream). Broker CLI accepts `--quic-delivery-strategy control-only|per-topic|per-publish`.

**Next**: Tune QUIC flow control (Phase 3).

## 2026-03-03 — Phase 1: DataPerSubscription deprecated as alias of DataPerTopic

**What**: Marked `StreamStrategy::DataPerSubscription` as `#[deprecated]`, merged client publish path to treat it identically to `DataPerTopic`, removed `send_on_subscription_stream()` method.

**Why**: `send_on_subscription_stream()` delegated directly to `send_on_topic_stream()` — they were identical code paths producing identical experiment results. Rather than implementing true per-subscription grouping (which has unclear semantics for the paper), deprecated the variant to make the alias explicit.

**Result**: CLI still accepts `--quic-stream-strategy per-subscription` for backwards compatibility but logs it as `per-topic`. Experiment results correctly labeled.

**Next**: Add broker delivery strategy support (Phase 2).

## 2026-02-28 — Root cause identified: publisher-side buffer bloat + shared congestion window

**What**: Traced the catastrophic per-topic latency (6-17s at 1% loss) to multi-stream buffer bloat.

**Why**: With 8 long-lived per-topic streams, each stream has its own send buffer in Quinn. Under loss, the shared congestion window shrinks but `publish().await` writes into per-stream buffers and returns quickly without blocking. Total buffering = 8x per-stream capacity before backpressure kicks in. Control-only has 1 stream = 1x buffer = faster backpressure = lower latency. Per-publish uses ephemeral streams = no buffer accumulation = best latency.

**Result**: This explains why per-topic delivers MORE messages (2217 msg/s) than TCP (1573 msg/s) but with 18x worse latency (6428ms vs 365ms). Messages are buffered, not blocked — classic buffer bloat.

**Next**: Reduce per-stream send buffer in Quinn transport config (Phase 3). Add broker delivery strategy support (Phase 2) so experiments can isolate publisher-side vs subscriber-side effects.

## 2026-02-28 — Code investigation: 5 hypotheses formulated

**What**: Investigated experiment 02 anomalies by reading source code of stream managers, bench tool, and broker publish path.

**Why**: per-topic/per-sub wcorr=1.0 under any loss was unexpected — should be < 1.0 if QUIC streams provide true independence.

**Result**: Five hypotheses confirmed:
- H1 (CONFIRMED): Multi-stream buffer bloat causes catastrophic latency — 8 streams x per-stream buffer = 8x total buffering before backpressure
- H2 (CONFIRMED): `send_on_subscription_stream()` delegates directly to `send_on_topic_stream()` — code bug makes them identical
- H3 (CONFIRMED): Broker always delivers via per-topic `ServerStreamManager` regardless of client's `--quic-stream-strategy` — design gap
- H4 (METHODOLOGY): wcorr conflates HOL blocking with congestion-induced correlation — metric limitation, not code bug
- H5 (EXPECTED): 60% delivery at 0% loss is normal QoS 0 behavior when subscriber can't keep up

**Next**: Implement fixes for H1-H3 in three phases.

## 2026-02-28 — Experiment 02 results reveal anomalies

**What**: Analyzed experiment 02 (HOL blocking) results across all stream strategies and loss rates.

**Why**: Expected per-topic and per-sub QUIC to show lower correlation than TCP under packet loss, indicating reduced HOL blocking.

**Result**: Anomalies found:
- per-topic/per-sub wcorr=1.000 under any loss (same as TCP — should be lower)
- per-topic latency 6-17s at 1% loss (10-20x worse than TCP's 365ms)
- per-topic delivers more msg/s than TCP BUT with catastrophically higher latency
- 0% loss delivers only ~3000/5000 msg/s for all transports (expected QoS 0 behavior)
- per-sub results are byte-identical to per-topic (confirms code alias bug)
- per-publish is the ONLY strategy that reduces correlation (wcorr 0.62-0.96)

**Next**: Investigate source code to identify root causes.
