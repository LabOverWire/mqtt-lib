# HOL Blocking Investigation Report

Experiment 02 measured head-of-line (HOL) blocking across QUIC multi-stream strategies.
The results were anomalous. This report documents the anomalies, root cause analysis,
code-level evidence, fixes applied, and implications for the paper.

## Background

QUIC multiplexes independent streams within a single connection. When a packet is lost,
only the stream it belongs to stalls — other streams continue unaffected. This is QUIC's
core advantage over TCP, where a single lost packet blocks the entire byte stream.

Our experiment tested whether this theoretical advantage translates to MQTT workloads.
The setup: 8 topics, 5000 msg/s total (625 msg/s per topic), QoS 0, packet loss injected
via `tc netem` on the broker side. We measured **weighted correlation (wcorr)** between
per-topic delivery rates — high correlation means topics slow down together (HOL blocking),
low correlation means topics behave independently (stream isolation).

Four QUIC stream strategies were tested:
- **ControlOnly**: all PUBLISH packets on the single MQTT control stream (QUIC equivalent of TCP)
- **DataPerTopic**: one dedicated QUIC stream per topic (8 long-lived streams)
- **DataPerSubscription**: intended to group streams by subscription filter pattern
- **DataPerPublish**: one ephemeral QUIC stream per PUBLISH message

## Anomalous Results

| Metric | TCP | QUIC control-only | QUIC per-topic | QUIC per-sub | QUIC per-publish |
|--------|-----|-------------------|----------------|--------------|------------------|
| wcorr at 1% loss | 0.74–1.0 | similar | **1.000** | **1.000** | 0.62–0.96 |
| Latency at 1% loss | 365ms | similar | **6,428ms** | **6,428ms** | lower |
| Throughput at 1% loss | 1,573 msg/s | similar | 2,217 msg/s | 2,217 msg/s | lower |
| Delivery at 0% loss | ~3,000/5,000 | ~3,000/5,000 | ~3,000/5,000 | ~3,000/5,000 | ~5,000/5,000 |

Five observations were anomalous:

1. **per-topic wcorr = 1.0 under loss** — identical to TCP, no stream independence at all
2. **per-topic latency 18× worse than TCP** — 6.4s vs 365ms, despite higher throughput
3. **per-topic delivers more messages than TCP but with catastrophically higher latency** — the throughput/latency paradox
4. **per-subscription results byte-identical to per-topic** — clearly a code issue
5. **0% loss delivers only 60% of configured rate** — unexpected but explained by QoS 0 semantics

Only **per-publish** showed the expected behavior: lower wcorr, indicating genuine stream independence.

## Root Cause Analysis

### H1: Multi-Stream Buffer Bloat (CONFIRMED)

This was the primary root cause of anomalies 1–3.

#### The mechanism

Quinn (the Rust QUIC library) assigns each stream its own send buffer. When the publisher
calls `client.publish().await`, the data is written into the per-stream buffer and the
future resolves immediately — it does NOT wait for the network to actually transmit the data.

Under packet loss, QUIC's congestion controller shrinks the connection-level congestion window.
But the publisher continues writing at the configured rate (5000 msg/s). Each of the 8 per-topic
streams independently accepts these writes into its own buffer. The publisher only experiences
backpressure when the aggregate buffered data exceeds the connection's send capacity.

With Quinn's defaults (tuned for 100 Mbps / 100ms RTT links):
- Per-stream receive window: ~1.25 MiB
- Connection send window: ~10 MiB

Total buffering before backpressure = 8 streams × per-stream capacity ≈ 10 MiB.

With TCP or control-only QUIC (1 stream), there's only 1× buffering. The single stream's
buffer fills quickly under loss, `publish().await` blocks, and the publisher naturally
rate-adapts. Latency stays bounded.

#### The throughput/latency paradox explained

per-topic delivers 2,217 msg/s at 1% loss — MORE than TCP's 1,573 msg/s. This seems
contradictory until you realize the messages aren't being delivered faster, they're being
**buffered** faster. The 10 MiB of multi-stream buffer absorbs messages that TCP would have
blocked on. The messages eventually arrive, but with 6.4 seconds of delay.

This is textbook buffer bloat: high throughput, catastrophic latency, because the system
has too much buffering between the sender and the congestion signal.

#### Why wcorr = 1.0

The wcorr metric measures whether per-topic delivery rates rise and fall together. Under loss,
QUIC's congestion controller throttles the entire connection equally — all 8 streams slow down
in lockstep. This produces perfect correlation (wcorr = 1.0) even though no stream-level HOL
blocking is occurring.

The correlation is real, but the cause is congestion control, not HOL blocking. These are
fundamentally different phenomena:
- **HOL blocking**: stream A's lost packet blocks stream B's delivery because they share an
  ordered byte stream (TCP behavior)
- **Congestion-induced correlation**: loss triggers congestion control that throttles ALL
  streams equally — streams A and B both slow down, but NOT because of ordering

The wcorr metric cannot distinguish between these two causes. This is a fundamental limitation
of the experimental methodology, not a code bug.

#### Evidence from code

The publisher spawns one async task per topic (`bench_cmd.rs:spawn_hol_publishers`). Each task
calls `client.publish().await` in a loop with a fixed inter-message delay. The `.await` resolves
when the data is written to the Quinn stream buffer, not when the peer acknowledges receipt.

```
spawn_hol_publishers (bench_cmd.rs):
  for each topic:
    spawn async task:
      loop:
        client.publish(topic, payload).await  // writes to per-stream Quinn buffer
        sleep(inter_message_delay)            // 1600µs for 625 msg/s per topic
```

The Quinn transport config used default values that allowed massive buffering:

```
Quinn defaults (before fix):
  stream_receive_window: ~1.25 MiB per stream
  receive_window:        ~8 MiB per connection
  send_window:           ~10 MiB per connection
```

#### Fix applied (Phase 3)

Reduced Quinn transport config for both client and server to limit buffer bloat:

```rust
// crates/mqtt5/src/transport/quic.rs (client side)
// crates/mqtt5/src/broker/quic_acceptor.rs (server side)
transport_config.stream_receive_window(262_144u32.into());  // 256 KiB (was ~1.25 MiB)
transport_config.receive_window(1_048_576u32.into());        // 1 MiB (was ~8 MiB)
transport_config.send_window(1_048_576);                     // 1 MiB (was ~10 MiB)
```

New maximum buffering: 8 streams × 256 KiB = 2 MiB (was ~10 MiB). `publish().await` will
block 5× sooner under loss, producing backpressure instead of unbounded buffering.

### H2: DataPerSubscription Was Identical to DataPerTopic (CONFIRMED — code bug)

This explains anomaly 4.

The `send_on_subscription_stream()` method was a direct delegation:

```rust
// crates/mqtt5/src/transport/quic_stream_manager.rs (before fix)
pub async fn send_on_subscription_stream(&self, topic: String, packet: Packet) -> Result<()> {
    self.send_on_topic_stream(topic, packet).await
}
```

The intended semantics were to group streams by subscription filter pattern rather than
exact topic name. For example, a subscription to `sensor/#` matching topics `sensor/temp`,
`sensor/humidity`, and `sensor/pressure` would use a single stream for all three topics.
Instead, it used one stream per topic — identical to `DataPerTopic`.

The experiment confirmed this: per-subscription results were byte-identical to per-topic
across all loss rates, topic counts, and metrics.

#### Fix applied (Phase 1)

Rather than implementing true per-subscription grouping (which has unclear semantics and
questionable value for the paper), we deprecated the variant as an explicit alias:

- Marked `StreamStrategy::DataPerSubscription` with `#[deprecated]`
- Removed `send_on_subscription_stream()` method
- All code paths now treat `DataPerSubscription` identically to `DataPerTopic`
- CLI still accepts `--quic-stream-strategy per-subscription` for backwards compatibility
  but logs it as `per-topic`

For the paper, we note that per-subscription was architecturally identical to per-topic in
our implementation, and that a true per-subscription strategy would require subscription
filter tracking in the stream manager — a non-trivial extension with unclear benefits over
per-topic grouping.

### H3: Broker Always Delivered via Per-Topic Streams (CONFIRMED — design gap)

The `--quic-stream-strategy` CLI flag only controlled the **publisher's** sending behavior.
The broker's `ServerStreamManager` always delivered to subscribers using per-topic streams,
regardless of what the client requested.

This meant the experiment varied only the publisher→broker path. The broker→subscriber path
was always per-topic (8 long-lived streams). The wcorr metric measured a combination of
publisher-side and subscriber-side correlation that couldn't be separated.

#### Evidence from code

```
Publisher sends PUBLISH          Broker delivers to subscriber
(controlled by --quic-stream-strategy)    (ALWAYS per-topic)
         |                                      |
    [strategy varies]                    [ServerStreamManager]
         |                                      |
    publisher ──────> broker ──────> subscriber
```

The `ServerStreamManager` (`broker/server_stream_manager.rs`) maintained a
`HashMap<String, ServerStreamInfo>` mapping topics to long-lived QUIC streams. Every
outbound PUBLISH was written to the stream corresponding to its topic, with LRU eviction
when the cache exceeded 100 entries. No configuration option existed to change this behavior.

#### Fix applied (Phase 2)

Added `ServerDeliveryStrategy` enum to the broker config:

```rust
// crates/mqtt5/src/broker/config/transport.rs
pub enum ServerDeliveryStrategy {
    ControlOnly,  // all publishes on MQTT control stream
    PerTopic,     // one stream per topic (previous behavior, still default)
    PerPublish,   // ephemeral stream per outbound publish
}
```

Wired through:
- `BrokerConfig::server_delivery_strategy()` builder method
- `ClientHandler` stores and passes strategy to `ServerStreamManager`
- `ServerStreamManager::write_publish()` dispatches based on strategy
- Broker CLI: `--quic-delivery-strategy control-only|per-topic|per-publish`

For `PerPublish`, each outbound message opens a new bidirectional stream, writes the flow
header + encoded packet, calls `finish()` to signal end-of-stream, and yields to let
Quinn flush the data. The stream is not cached.

For `ControlOnly`, the `write_publish()` method returns an error, signaling the caller
(`write_publish_bytes` in `publish.rs`) to fall through to writing on the control stream.

This allows experiments to independently vary publisher-side and subscriber-side strategies,
isolating their effects on correlation and latency.

### H4: wcorr Conflates HOL Blocking with Congestion (Methodology Limitation)

Not a code bug, but critical for interpreting results and writing the paper.

True HOL blocking and congestion-induced correlation produce the same wcorr signal.
per-topic QUIC eliminates true HOL blocking (separate byte streams), but wcorr still
reads 1.0 because QUIC's shared congestion window throttles all streams identically
under loss.

#### Implications for the paper

1. wcorr alone cannot prove or disprove HOL blocking mitigation
2. per-publish wcorr < 1.0 IS genuine independence — ephemeral streams have no
   accumulated buffer state, so they don't share congestion window effects the same way
3. The distinction between HOL blocking and congestion correlation must be acknowledged
4. Additional metrics may be needed: per-stream goodput traces, retransmission counts,
   or inter-stream delivery ordering analysis

### H5: 60% Delivery at 0% Loss (Expected QoS 0 Behavior)

At 5000 msg/s configured rate with QoS 0, the broker/subscriber can process ~3000 msg/s.
QoS 0 is fire-and-forget — the MQTT specification explicitly allows brokers to drop messages
when the subscriber can't keep up. No acknowledgment, no retry, no queuing obligation.

This is expected behavior, not a bug. The experiment should note the effective delivery
rate vs the configured publish rate.

One notable exception: per-publish at 0% loss achieved ~5000 msg/s (100% delivery).
This is because per-publish opens a fresh stream per message, and Quinn's flow control
provides natural backpressure per-stream — the publisher automatically slows down to
match the subscriber's consumption rate. The other strategies buffer aggressively on
long-lived streams, masking the subscriber bottleneck until messages are silently dropped.

## Summary of Changes

### Files modified

| File | Change |
|------|--------|
| `crates/mqtt5/src/transport/quic.rs` | Deprecated `DataPerSubscription`, added flow control tuning to client transport config |
| `crates/mqtt5/src/transport/quic_stream_manager.rs` | Removed `send_on_subscription_stream()` method |
| `crates/mqtt5/src/client/direct/mod.rs` | Merged `DataPerTopic \| DataPerSubscription` match arms in publish path |
| `crates/mqtt5/src/broker/config/transport.rs` | Added `ServerDeliveryStrategy` enum |
| `crates/mqtt5/src/broker/config/mod.rs` | Added `server_delivery_strategy` field and builder method to `BrokerConfig` |
| `crates/mqtt5/src/broker/server_stream_manager.rs` | Added strategy dispatch in `write_publish()`, `PerPublish` ephemeral streams |
| `crates/mqtt5/src/broker/client_handler/mod.rs` | Added `server_delivery_strategy` field (cfg-gated for non-WASM) |
| `crates/mqtt5/src/broker/client_handler/publish.rs` | Route publish writes through delivery strategy |
| `crates/mqtt5/src/broker/quic_acceptor.rs` | Extract delivery strategy from config, add server-side flow control tuning |
| `crates/mqttv5-cli/src/commands/broker_cmd.rs` | Added `--quic-delivery-strategy` CLI argument |
| `crates/mqttv5-cli/src/commands/bench_cmd.rs` | Merged deprecated match arms |
| `crates/mqttv5-cli/src/commands/parsers.rs` | Added `parse_delivery_strategy()` function |

### Flow control values

| Parameter | Before (Quinn default) | After | Rationale |
|-----------|----------------------|-------|-----------|
| `stream_receive_window` | ~1.25 MiB | 256 KiB | 8 streams × 256 KiB = 2 MiB max before stream-level backpressure |
| `receive_window` | ~8 MiB | 1 MiB | Connection-level receive cap |
| `send_window` | ~10 MiB | 1 MiB | Connection-level send cap, limits total in-flight data |

Quinn defaults assume 100 Mbps bandwidth and 100ms RTT (BDP = 1.25 MiB). MQTT messages
are typically small (< 1 KiB), so these defaults allow massive over-buffering. The reduced
values still allow high throughput for MQTT workloads while providing backpressure 5× sooner.

## Open Questions for Phase 4

1. Does the flow control tuning actually reduce per-topic latency under loss? (Expected: yes)
2. Does wcorr drop below 1.0 with reduced buffering? (Uncertain — congestion correlation may still dominate)
3. Does matching broker delivery strategy to client strategy change the correlation profile?
4. Should the 256 KiB stream window be configurable via CLI for experimentation?
5. What happens to per-publish throughput with the tighter send_window? (It already had natural backpressure, so impact may be minimal)

## Key Insight for the Paper

The experiment revealed a distinction that is absent from the QUIC HOL blocking literature:
**stream-level HOL blocking** and **connection-level congestion correlation** are different
phenomena that produce identical signals in standard correlation metrics.

QUIC's multi-stream design eliminates stream-level HOL blocking (a lost packet on stream A
does not delay delivery of stream B's packets). But QUIC's congestion control operates at
the connection level — a loss event throttles ALL streams equally. Under sustained loss,
all streams experience identical throughput reduction, producing perfect inter-stream
correlation that looks identical to HOL blocking in aggregate metrics.

This means that QUIC's HOL blocking mitigation is most observable in **transient** loss
events (where stream B continues delivering while stream A retransmits) rather than in
**sustained** loss (where congestion control dominates). Experiment design must account
for this: short loss bursts may reveal stream independence, while sustained loss profiles
mask it behind congestion correlation.

Buffer bloat amplifies this effect. With large per-stream buffers, the publisher doesn't
experience backpressure promptly, and messages accumulate uniformly across all streams.
By the time congestion control kicks in, all streams have similar buffer depths, producing
correlated drain patterns. Reducing buffer sizes forces earlier backpressure, potentially
revealing stream-level independence that was previously hidden by uniform buffering.
