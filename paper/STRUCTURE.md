# MQoQ: A Complete Implementation of Advanced Multistream MQTT over QUIC

## Paper Structure

---

### Abstract (~250 words)

- MQTT is the dominant IoT messaging protocol but relies on TCP, inheriting head-of-line blocking, slow reconnection, and no native multiplexing
- The OASIS MQTT Technical Committee has drafted an MQTT-over-QUIC (MQoQ) specification defining three operational modes, but no complete implementation of the advanced multistream mode exists
- We present mqtt5, an open-source Rust implementation that realizes the full MQoQ Advanced Multistream specification: four configurable stream mapping strategies, flow-level session persistence, QUIC datagram delivery for unreliable QoS 0, server-initiated flows, and runtime strategy switching
- We evaluate our implementation against TCP baselines and single-stream QUIC across throughput, latency, reconnection time, and head-of-line blocking resistance under varying packet loss conditions
- Results show [placeholder for key findings]

---

### 1. Introduction (~1.5 pages)

1.1 MQTT's dominance in IoT and its TCP limitations
- Head-of-line blocking in multiplexed topic streams
- Reconnection overhead (TCP + TLS 1.2 handshake = 2-3 RTTs minimum)
- No native stream multiplexing — all topics share one ordered byte stream
- Connection migration impossible (IP change = full reconnect)

1.2 QUIC as a transport for IoT
- Built-in TLS 1.3 (1-RTT, 0-RTT resumption)
- Independent streams eliminate HOL blocking
- Connection migration via connection IDs
- Unreliable datagrams (RFC 9221)

1.3 The gap
- Existing implementations treat QUIC as a TCP drop-in (single stream)
- EMQX provides partial multi-stream but lacks advanced features (flow persistence, datagrams, server-initiated flows)
- No independent implementation of the OASIS MQoQ draft spec exists
- No empirical comparison of stream mapping strategies in the literature

1.4 Contributions
- First complete implementation of MQoQ Advanced Multistream (Mode 3)
- Four stream mapping strategies with runtime switching
- Flow-level session persistence via 8-bit flag bitfield
- QUIC datagram integration for unreliable QoS 0
- Empirical evaluation comparing strategies under realistic IoT conditions

---

### 2. Background and Related Work (~2 pages)

2.1 MQTT v5.0 protocol overview
- Publish/subscribe model, QoS levels 0/1/2
- Session state, topic aliases, flow control
- Properties system and user properties

2.2 QUIC transport protocol
- RFC 9000 overview: streams, flow control, connection migration
- RFC 9221: unreliable datagrams
- QUIC vs TCP+TLS: handshake reduction, HOL blocking elimination
- Stream types: bidirectional, unidirectional, client/server-initiated

2.3 Prior work on MQTT over QUIC
- Kumar & Dezfouli (2019): single-stream transport replacement, 56% connection overhead reduction
- Fernandez et al. (2020): single-stream Go implementation, up to 40% improvement over TCP
- Sensors (2022): empirical characterization on COTS devices
- Schoeffmann et al. (2023): HTTP/3 alternative to MQTT-over-QUIC
- EMQX: production multi-stream (Erlang/MsQuic), partial Mode 2 compliance
- RMQTT: Rust single-stream QUIC listener
- MQuicTT: experimental Rust, pre-alpha, incorrect QoS2 claims

2.4 OASIS MQoQ draft specification
- Three operational modes (Single Stream, Simple Multistreams, Advanced Multistreams)
- Flow types: Control (0x11), Client Data (0x12), Server Data (0x13), User-Defined (0x14)
- Flow persistence flags and lifecycle state machine
- Current status: early draft, single-vendor authorship (EMQ)

2.5 Gap analysis table
- Summary table: implementation vs. spec features across all known systems

---

### 3. System Architecture (~3 pages)

3.1 Design principles
- Rust async/await with Tokio runtime — zero-cost async abstractions
- Shared state via Arc<RwLock<T>> — no message-passing overhead
- Quinn QUIC library with rustls ring crypto provider
- ALPN negotiation with "mqtt" protocol identifier

3.2 Transport abstraction layer
- Unified transport trait supporting TCP, TLS, WebSocket, and QUIC
- PacketReader/PacketWriter traits for stream-agnostic MQTT packet I/O
- Connection URL scheme routing: tcp://, tls://, ws://, wss://, quic://, quics://

3.3 Stream mapping strategies

```
┌─────────────────────────────────────────────────────┐
│                  QUIC Connection                     │
├──────────┬──────────┬──────────┬────────────────────┤
│ Strategy │ Control  │ Data     │ Datagrams          │
├──────────┼──────────┼──────────┼────────────────────┤
│ Control  │ 1 bidi   │ none     │ optional (QoS 0)   │
│ Only     │ stream   │          │                    │
├──────────┼──────────┼──────────┼────────────────────┤
│ Per-     │ 1 bidi   │ 1 bidi   │ optional (QoS 0)   │
│ Publish  │ stream   │ per msg  │                    │
├──────────┼──────────┼──────────┼────────────────────┤
│ Per-     │ 1 bidi   │ 1 bidi   │ optional (QoS 0)   │
│ Topic    │ stream   │ per topic│                    │
├──────────┼──────────┼──────────┼────────────────────┤
│ Per-Sub  │ 1 bidi   │ 1 bidi   │ optional (QoS 0)   │
│          │ stream   │ per sub  │                    │
└──────────┴──────────┴──────────┴────────────────────┘
```

- ControlOnly: minimal overhead, all packets on single stream, datagrams for QoS 0
- DataPerPublish: maximum parallelism, one stream per PUBLISH, flow header per stream
- DataPerTopic: LRU stream cache (100 max), 300s idle timeout, topic-to-stream affinity
- Runtime switching without reconnection

3.4 MQoQ flow header protocol
- Flow type bytes (0x11–0x14) as first byte on data streams
- Flow ID encoding: 64-bit with LSB client/server indicator
- FlowFlags 8-bit bitfield: persistent_qos, persistent_subscriptions, persistent_topic_alias, err_tolerance (2-bit), clean, abort_if_no_state, optional_headers
- Flow expiration interval
- Lazy header parsing (up to 32 bytes) with leftover passthrough to packet decoder

3.5 Flow registry and lifecycle management
- Per-connection FlowRegistry (256 max flows)
- FlowState: ID, type, flags, expiration, subscriptions, topic aliases, pending packet IDs
- State machine: Idle → Open → HalfClosed → Closed
- Invalid transition prevention
- Automatic expiration based on inactivity

3.6 QUIC datagram integration
- RFC 9221 unreliable datagrams for QoS 0 PUBLISH
- Configurable send/receive buffers (64KB default)
- Broker-side datagram reader task feeds into packet channel
- Tradeoff: sub-millisecond delivery vs. no reliability guarantee

3.7 Server-initiated flows
- Broker can open data streams toward client (flow type 0x13)
- Enables server-assigned subscriptions and push scenarios
- Client-side stream acceptance task processes incoming broker streams

3.8 Broker architecture
- Multi-transport acceptor: TCP, TLS, WebSocket, QUIC on distinct ports
- Per-connection handler spawns 3 tasks: main handler, datagram reader, stream acceptor
- Flow header detection on data streams (first-byte inspection)
- Cluster-to-cluster QUIC connections with optional mTLS
- Bridge support for QUIC as upstream transport

---

### 4. Implementation Details (~1.5 pages)

4.1 Packet I/O over QUIC streams
- BytesMut buffering with zero-copy Bytes slices for send path
- Variable-length remaining-length parsing per MQTT v5.0 spec
- Per-stream async task with internal buffering
- mpsc channel aggregation for multi-stream → single handler

4.2 QuicStreamManager
- Configurable max cached streams
- Automatic idle stream cleanup
- Per-stream flow header injection
- tokio::task::yield_now() for I/O driver transmission batching

4.3 Client API surface
- Runtime-configurable: strategy, flow headers, expiration, max streams, datagrams
- ConnectOptions builder pattern for QUIC configuration
- CLI tool integration with full QUIC parameter exposure

4.4 Security considerations
- TLS 1.3 mandatory (QUIC requirement)
- ALPN protocol negotiation prevents protocol confusion
- Optional mTLS for mutual authentication
- Certificate chain and custom CA support
- x-mqtt-sender / x-mqtt-client-id injection for identity tracking

---

### 5. Experimental Evaluation (~3 pages)

5.1 Experimental setup
- Hardware: [specify test machines]
- Network conditions: LAN, emulated WAN (netem), varying RTT (5ms, 50ms, 200ms, 600ms)
- Packet loss rates: 0%, 1%, 5%, 10%, 15%
- Message sizes: 64B (sensor telemetry), 1KB (events), 10KB (images), 100KB (firmware chunks)
- Topic counts: 1, 10, 100, 1000
- Client counts: 1, 10, 100

5.2 Baseline comparisons
- MQTT over TCP (no TLS)
- MQTT over TLS 1.2
- MQTT over TLS 1.3
- MQTT over QUIC (single-stream / ControlOnly)

5.3 Experiment 1: Connection establishment latency
- TCP+TLS vs. QUIC initial handshake
- Reconnection latency (session resumption)
- Across RTT conditions (LAN → satellite)
- Metric: time from connect() to CONNACK received

5.4 Experiment 2: Head-of-line blocking resistance
- Multiple topics with concurrent publishers
- Inject packet loss on one topic's stream
- Measure cross-topic latency impact
- Compare: TCP (total HOL), ControlOnly (partial HOL), PerTopic (isolated HOL)
- This is the key differentiating experiment — no prior work has measured this

5.5 Experiment 3: Throughput under loss
- Sustained publish at varying rates
- Packet loss from 0% to 15%
- Compare all four strategies + TCP baseline
- QoS 0, QoS 1, QoS 2 separately
- Include datagram mode for QoS 0

5.6 Experiment 4: Stream strategy comparison
- ControlOnly vs. DataPerPublish vs. DataPerTopic
- Metrics: throughput, latency (p50/p95/p99), memory usage, stream count
- Varying topic counts (1 → 1000)
- Characterize crossover points where strategies change relative performance

5.7 Experiment 5: Datagram mode for QoS 0
- Reliable stream vs. unreliable datagram for QoS 0 PUBLISH
- Latency comparison under congestion
- Delivery rate under packet loss
- Message size limits (QUIC datagram MTU constraints)

5.8 Experiment 6: Resource overhead
- Memory footprint per connection (TCP vs. QUIC, per strategy)
- CPU usage under load
- Stream creation/teardown overhead
- Flow registry memory impact

---

### 6. Discussion (~1 page)

6.1 Strategy selection guidelines
- When to use each strategy (decision tree based on workload characteristics)
- ControlOnly: low-topic-count, latency-sensitive, resource-constrained
- DataPerPublish: high-parallelism, independent messages, bursty traffic
- DataPerTopic: many topics, mixed QoS, HOL blocking concern

6.2 Spec compliance observations
- Areas where OASIS MQoQ draft is ambiguous or underspecified
- Implementation decisions where spec left room for interpretation
- Feedback for spec authors based on implementation experience

6.3 Comparison with EMQX
- Feature comparison table
- Architectural differences (Erlang/OTP vs. Rust async)
- Features we implement that EMQX doesn't (flow persistence, datagrams, server-initiated)

6.4 Limitations
- No 0-RTT support yet (Quinn API not exposed)
- No connection migration utilization (QUIC supports it, not leveraged for MQTT session)
- 256 max flows per connection (configurable but bounded)
- Datagram MTU limits message size for QoS 0 datagram mode

---

### 7. Conclusion and Future Work (~0.5 page)

7.1 Summary of contributions
- Complete MQoQ Advanced Mode implementation
- Four stream strategies with empirical characterization
- Open-source Rust implementation

7.2 Future work
- 0-RTT connection resumption
- QUIC connection migration for MQTT session mobility
- QPACK header compression for flow headers
- Formal verification of flow state machine
- Interoperability testing with EMQX
- Contribution to OASIS MQoQ standardization process

---

### References (~30-40 entries)

RFC 9000, RFC 9001, RFC 9221, MQTT v5.0 OASIS Standard, MQoQ draft spec,
Kumar 2019, Fernandez 2020, Sensors 2022, Schoeffmann 2023,
EMQX documentation, Quinn crate, rustls, tokio

---

## Figures Plan

1. **Fig 1**: MQTT over TCP vs. MQTT over QUIC architectural comparison (connection-level)
2. **Fig 2**: Four stream strategy diagrams (side by side)
3. **Fig 3**: MQoQ flow header format (byte-level diagram)
4. **Fig 4**: Flow lifecycle state machine (Idle → Open → HalfClosed → Closed)
5. **Fig 5**: Broker architecture — multi-transport acceptor with QUIC task decomposition
6. **Fig 6**: Connection establishment latency (bar chart, TCP/TLS/QUIC × RTT conditions)
7. **Fig 7**: HOL blocking experiment — cross-topic latency under per-topic loss (the key figure)
8. **Fig 8**: Throughput under loss (line chart, strategies × loss rates)
9. **Fig 9**: Strategy comparison — latency percentiles across topic counts (heatmap or grouped bars)
10. **Fig 10**: Datagram vs. stream QoS 0 latency comparison
11. **Fig 11**: Resource overhead — memory and CPU per strategy

## Tables Plan

1. **Table 1**: Feature comparison across all known MQTT-over-QUIC implementations
2. **Table 2**: MQoQ specification modes vs. implementation coverage
3. **Table 3**: FlowFlags bitfield definition
4. **Table 4**: Experimental parameters summary
5. **Table 5**: Strategy selection decision matrix
