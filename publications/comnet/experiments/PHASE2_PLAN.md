# Phase 2 Experiment Plan

Phase 1 exposed a fundamental subscriber bottleneck (64 publishers, 1 subscriber).
Measured "throughput" was actually subscriber consumption rate, not transport ceiling.
Phase 2 fixes this with balanced topologies and adds flow headers testing.

## Lessons from Phase 1

- 64:1 pub:sub ratio means publishers always outrun the subscriber
- Packet loss "improves" received throughput by throttling publishers toward subscriber capacity
- QUIC per-publish was the only strategy with natural backpressure (100% delivery at 0% loss)
- QUIC per-topic achieves genuine HOL independence (correlation 0.66) but with 2-4x worse absolute latency than control-only
- QUIC control-only has best absolute latency under loss (3.7x better than TCP at 5% loss)
- Broker RSS hit 2.6 GB during QoS 1 flood — need to verify it recovers (allocator retention vs leak)
- Flow headers (`--quic-flow-headers`) were never tested

## Phase 2 Experiments

### 07: Balanced Throughput (1:1 pub:sub per topic)

64 topics, 64 publishers, 64 subscribers (one per topic).
No subscriber bottleneck — measures true transport throughput ceiling.

- Transports: TCP, QUIC control-only, per-topic, per-subscription, per-publish
- Loss: 0%, 1%, 2%, 5%
- QoS: 0, 1
- Payload: 256 bytes
- Duration: 30s, warmup 5s, 5 runs

Key questions:
- What's the actual throughput ceiling per transport without subscriber bottleneck?
- Does per-topic still have worse absolute latency when subscribers aren't starved?
- How does QoS 1 throughput compare when PUBACK isn't competing with a flood?

### 08: Fan-out (1 publisher, N subscribers)

1 publisher, N subscribers all on same topic. Tests broker multicast efficiency.

- Subscriber counts: 1, 10, 50, 100
- Transports: TCP, QUIC control-only, per-subscription
- Loss: 0%, 2%, 5%
- QoS: 0
- Payload: 256 bytes
- Duration: 30s, warmup 5s, 5 runs

Key questions:
- How does throughput scale with subscriber count?
- Does QUIC per-subscription isolate slow subscribers from fast ones?
- Resource overhead per subscriber (RSS, CPU)?

### 09: HOL Blocking with Per-Topic Subscribers

64 topics, 64 publishers, 64 subscribers (one per topic).
Phase 1 HOL used 1 subscriber for all 64 topics — all topics shared one receive path.
With per-topic subscribers, QUIC per-topic streams should show true independence.

- Transports: TCP, QUIC control-only, per-topic, per-subscription, per-publish
- Loss: 0%, 1%, 2%, 5%
- Delay: 25ms
- Payload: 512 bytes
- Duration: 30s, warmup 5s, 5 runs

Key questions:
- Does per-topic correlation drop further with independent subscribers?
- Do absolute latencies improve when each subscriber only handles one topic?

### 10: Flow Headers Impact

Same as experiment 07 (balanced throughput) but with `--quic-flow-headers` enabled.
Compare against experiment 07 results directly.

- Transports: QUIC per-topic, per-subscription (flow headers are irrelevant for control-only/per-publish)
- Loss: 0%, 1%, 2%, 5%
- QoS: 0, 1
- Payload: 256 bytes
- Duration: 30s, warmup 5s, 5 runs

Key questions:
- Do flow headers improve throughput or latency for multi-stream strategies?
- Is there measurable overhead from the header bytes themselves at 0% loss?
- Under loss, do flow headers help the broker recover stream state faster?

### 11: Memory Recovery Under Load Cycling

Single long-running broker, alternating between heavy load and idle periods.
Tests whether RSS recovers or ratchets up (allocator retention vs leak).

- Cycle: 60s heavy load (64 pub QoS 1), 30s idle, repeat 10 times
- Transports: TCP, QUIC per-topic
- Loss: 5% (to trigger retransmission buffers)
- Monitor RSS throughout, report peak and trough per cycle

Key question:
- Does peak RSS grow across cycles (leak) or stabilize (allocator retention)?

## Infrastructure Notes

- Same GCP setup: broker 34.83.187.89, client 136.118.88.105 (e2-standard-4)
- Telemetry: per-run broker + client CSV (1s sampling)
- May need bench tool changes for 64-subscriber mode and load cycling
- `--quic-flow-headers` flag already exists on `mqttv5 bench`
