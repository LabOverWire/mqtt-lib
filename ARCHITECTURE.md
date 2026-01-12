# MQTT v5.0 Platform Architecture

This document describes the architecture of the MQTT client library, broker implementation, and protocol crate.

## Crate Organization

Four crates provide platform-specific implementations sharing a common protocol core:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         mqtt5-protocol                                   │
│                    (no_std + alloc compatible)                          │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌──────────────┐   │
│  │   Packets    │ │  Properties  │ │   Session    │ │  Validation  │   │
│  └──────────────┘ └──────────────┘ └──────────────┘ └──────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
          │                    │                    │
          ▼                    ▼                    ▼
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│      mqtt5       │  │    mqtt5-wasm    │  │   mqttv5-cli     │
│  (std + Tokio)   │  │ (WASM + browser) │  │  (CLI binary)    │
│                  │  │                  │  │                  │
│ - Native broker  │  │ - Browser client │  │ - pub/sub cmds   │
│ - TCP/TLS/QUIC   │  │ - In-tab broker  │  │ - broker cmd     │
│ - Full features  │  │ - MessagePort    │  │ - acl/passwd     │
└──────────────────┘  └──────────────────┘  └──────────────────┘
```

### mqtt5-protocol (Platform-Agnostic Core)

Platform-agnostic MQTT v5.0 protocol for native, WASM, and embedded targets. Supports `no_std` environments with `alloc`.

**Modules:**
- `packet/` - All MQTT v5.0 packet types (CONNECT, PUBLISH, SUBSCRIBE, etc.)
- `encoding/` - Binary encoding/decoding (variable integers, strings, binary data)
- `protocol/v5/` - Properties and reason codes
- `session/` - Session management primitives:
  - `flow_control` - QoS flow control configuration and stats
  - `limits` - Connection limits and message expiry
  - `queue` - Message queue with priority and expiry
  - `subscription` - Subscription state management
  - `topic_alias` - Topic alias mapping
- `validation/` - Topic validation and namespace rules
- `time` - Platform-abstracted time (std, WASM web-time, embedded fallback)
- `prelude` - Alloc/std compatibility layer

**Features:**
| Feature | Description |
|---------|-------------|
| `std` (default) | Full std support with thiserror, tracing |

For single-core targets, use cfg: `rustflags = ["--cfg", "portable_atomic_unsafe_assume_single_core"]`

**Dependencies:** `bebytes`, `bytes`, `serde`, `hashbrown`, `portable-atomic`, `portable-atomic-util`

**Optional:** `thiserror` (std), `tracing` (std), `web-time` (WASM)

### mqtt5 (Native)

Full-featured async client and broker for Linux, macOS, Windows.

**Client Features:**
- `MqttClient` with automatic reconnection and exponential backoff
- QoS 0/1/2 with proper flow control
- TLS (rustls) with CA and client certificate support
- QUIC multistream for parallel operations
- Enhanced authentication (SCRAM-SHA-256, JWT, custom handlers)
- Connection event callbacks

**Broker Features:**
- Multi-transport: TCP, TLS, WebSocket, QUIC on different ports
- Authentication providers: password (argon2), certificate, JWT, federated JWT
- ACL system with wildcard topic matching
- Broker-to-broker bridging with loop prevention
- File-based and in-memory storage backends
- $SYS topics for statistics
- Session takeover semantics
- Optional OpenTelemetry integration

**Session Module (extends protocol):**
- `quic_flow` - QUIC stream flow registry
- `retained` - Retained message store
- `state` - Full session state with async support

**Dependencies:** `tokio`, `rustls`, `tokio-tungstenite`, `quinn`

### mqtt5-wasm (WebAssembly)

Client and broker for browser environments. Published to npm as `mqtt5-wasm`.

```bash
npm install mqtt5-wasm
```

- `WasmMqttClient` with JavaScript Promise API
- `WasmBroker` for in-browser testing
- WebSocket, MessagePort, BroadcastChannel transports
- Single-threaded state (`Rc<RefCell<T>>`)

**Dependencies:** `wasm-bindgen`, `web-sys`, `js-sys`

### mqttv5-cli (Command-Line Tool)

Unified CLI for MQTT operations.

**Commands:**
- `mqttv5 pub` - Publish messages
- `mqttv5 sub` - Subscribe to topics
- `mqttv5 broker` - Run MQTT broker
- `mqttv5 acl` - Manage access control lists
- `mqttv5 passwd` - Manage password files
- `mqttv5 scram` - SCRAM credential management
- `mqttv5 bench` - Performance benchmarking

**Dependencies:** `clap`, `tokio`, `dialoguer`, `argon2`

## Embedded Target Support

The protocol crate supports embedded targets via `no_std`:

| Target | Command | Notes |
|--------|---------|-------|
| Cortex-M4 (ARM) | `--target thumbv7em-none-eabihf` | Has hardware atomics |
| RISC-V (atomics) | `--target riscv32imac-unknown-none-elf` | Has atomic extension |
| ESP32-C3 | `--target riscv32imc-unknown-none-elf` | Configure single-core via .cargo/config.toml |

For single-core targets without hardware atomics, add to `.cargo/config.toml`:
```toml
[target.riscv32imc-unknown-none-elf]
rustflags = ["--cfg", "portable_atomic_unsafe_assume_single_core"]
```

Build commands:
```bash
cargo make embedded-cortex-m4   # ARM Cortex-M4
cargo make embedded-riscv       # RISC-V with atomics
cargo make embedded-verify      # All embedded targets
```

## Core Architectural Principle: Direct Async/Await

This library uses Rust's native async/await patterns throughout:

1. Tokio provides the async runtime (native)
2. Direct async calls are efficient and idiomatic
3. Code is simpler to debug than channel-based architectures

## Client Architecture

### Core Components

1. **MqttClient**: Main client struct
   - Holds shared state (transport, session, callbacks)
   - Direct async methods for all operations
   - `Arc<RwLock<T>>` for concurrent access

2. **Transport Layer**: Direct async I/O
   - `read_packet()` - async method for incoming packets
   - `write_packet()` - async method for outgoing packets
   - Implementations: TCP, TLS, WebSocket, QUIC

3. **Background Tasks**:
   - Packet reader: Continuously reads and dispatches packets
   - Keep-alive: Sends PINGREQ at intervals
   - Reconnection: Exponential backoff recovery

4. **TLS Configuration**:
   - Stored config for CA certs and client certificates
   - Applied automatically for `mqtts://` URLs
   - Supports AWS IoT ALPN

### Data Flow

```
Incoming:  Network -> Transport.read_packet() -> packet_reader_task -> handle_packet() -> callbacks
Outgoing:  Client method -> Transport.write_packet() -> Network
```

### Error Handling

The client validates acknowledgment reason codes:
- PUBACK (QoS 1): Returns `MqttError::PublishFailed(reason_code)` on error
- PUBREC/PUBCOMP (QoS 2): Validates complete handshake
- Authorization: `ReasonCode::NotAuthorized` (0x87) from ACL failures

## Broker Architecture

### Core Components

1. **MqttBroker**: Main broker struct
   - Manages configuration and lifecycle
   - Spawns listening tasks per transport

2. **Server Listeners**: One per transport
   - TCP: Direct `accept()` loop
   - TLS: rustls wrapper with certificate validation
   - WebSocket: HTTP upgrade with tokio-tungstenite
   - QUIC: quinn endpoint with multistream

3. **ClientHandler**: Per-client connection
   - Direct async packet reading/writing
   - Manages client session state
   - Handles MQTT protocol

4. **MessageRouter**: Subscription matching
   - MQTT-compliant topic matching with wildcards (`+`, `#`)
   - System topic protection (`$SYS/#` excluded from `#`)
   - Shared subscription support (`$share/group/topic`)

5. **Storage Backend**: Persistence layer
   - Sessions, retained messages, queued messages
   - File-based or in-memory implementations

### Broker Data Flow

```
Connection:  Listener -> accept() -> spawn(ClientHandler)
Processing:  Client -> read_packet() -> handle_packet() -> Router/Storage
Routing:     Publisher -> Router.route_message() -> subscribers -> write_packet()
```

### Authentication System

Pluggable providers via `AuthProvider` trait:
- `AllowAllAuthProvider` - No authentication (development)
- `PasswordAuthProvider` - File-based with argon2 hashing
- `CertificateAuthProvider` - Client certificate validation
- `JwtAuthProvider` - JWT token validation with JWKS support
- `FederatedJwtAuthProvider` - Multi-issuer JWT support
- `ComprehensiveAuthProvider` - Combines multiple methods

Enhanced authentication mechanisms:
- SCRAM-SHA-256 with channel binding
- JWT with custom claims extraction
- PLAIN over TLS

### ACL System

Rule-based access control:
- Wildcard topic matching in rules
- Publish/subscribe permission separation
- Role-based access control (RBAC)
- CLI management: `mqttv5 acl add/remove/list/check`

### Bridge Manager

Broker-to-broker connections:
- Each bridge is a client to remote broker
- Topic mappings with prefix transformation
- Loop prevention via bridge headers
- TLS/mTLS support with AWS IoT integration
- Exponential backoff reconnection

### Resource Monitor

- Tracks connections, bandwidth, messages
- Enforces rate limits and quotas
- Direct checks, no monitoring loops

### Event Hooks

Custom event handlers via `BrokerEventHandler` trait:

| Hook | Event Type | Trigger |
|------|------------|---------|
| `on_client_connect` | `ClientConnectEvent` | Client CONNECT packet accepted |
| `on_client_subscribe` | `ClientSubscribeEvent` | Client SUBSCRIBE processed |
| `on_client_unsubscribe` | `ClientUnsubscribeEvent` | Client UNSUBSCRIBE processed |
| `on_client_publish` | `ClientPublishEvent` | Client PUBLISH received (includes `response_topic`, `correlation_data` for request/response) |
| `on_client_disconnect` | `ClientDisconnectEvent` | Client disconnects (clean or unexpected) |
| `on_retained_set` | `RetainedSetEvent` | Retained message stored or cleared |
| `on_message_delivered` | `MessageDeliveredEvent` | QoS 1/2 message delivered to subscriber |

Usage: `BrokerConfig::default().with_event_handler(Arc::new(handler))`

## QUIC Transport Architecture

QUIC provides MQTT over QUIC (RFC 9000) with multistream support:

```
Client                                    Broker
  │                                          │
  │──── QUIC Connection (TLS 1.3) ──────────│
  │                                          │
  │  Stream 0 (Control)                      │
  │  ├── CONNECT/CONNACK                     │
  │  ├── SUBSCRIBE/SUBACK                    │
  │  └── PINGREQ/PINGRESP                    │
  │                                          │
  │  Stream 2+ (Data - client initiated)     │
  │  └── PUBLISH (QoS 0/1/2)                 │
  │                                          │
  │  Stream 3+ (Data - server initiated)     │
  │  └── PUBLISH (subscribed messages)       │
```

### Stream Strategies

1. **ControlOnly**: Single stream (traditional MQTT behavior)
2. **DataPerPublish**: New stream per QoS 1/2 publish
3. **DataPerTopic**: Stream pooling by topic
4. **DataPerSubscription**: Dedicated streams per subscription

### Benefits

- No head-of-line blocking
- Parallel QoS flows
- Connection migration support
- Built-in TLS 1.3

## WASM Architecture

### Adaptations for Browser

1. **Single-Threaded**: `Rc<RefCell<T>>` instead of `Arc<Mutex<T>>`
2. **Async Bridge**: Rust async → JavaScript Promises
3. **No File I/O**: Memory-only storage
4. **Browser TLS**: `wss://` handled by browser

### WASM Client

```rust
pub struct WasmMqttClient {
    state: Rc<RefCell<ClientState>>
}
```

- Connection: `connect(url)`, `connect_message_port(port)`, `connect_broadcast_channel(name)`
- Publishing: `publish()`, `publish_qos1()`, `publish_qos2()`
- Subscription: `subscribe_with_callback(topic, callback)`
- Events: `on_connect()`, `on_disconnect()`, `on_error()`

### WASM Broker

Complete in-browser broker:
- Full MQTT v5.0 protocol
- MessagePort for in-tab clients
- Memory-only storage
- `create_client_port()` creates MessageChannel

### Browser Transports

1. **WebSocket**: External broker via `web_sys::WebSocket`
2. **MessagePort**: In-tab broker (zero network overhead)
3. **BroadcastChannel**: Cross-tab messaging

## Telemetry (Optional)

OpenTelemetry integration behind `opentelemetry` feature:

```
Publisher -> inject traceparent into user properties
         -> PUBLISH packet with trace context
         -> Broker extracts context, creates span
         -> Bridge forwards properties
         -> Subscriber extracts context
```

Configuration via `TelemetryConfig` and `BrokerConfig::with_opentelemetry()`.

## Testing Architecture

1. **Unit Tests**: Direct component testing
2. **Integration Tests**: Full client-broker with real connections
3. **Turmoil Tests**: Network simulation for failure scenarios
4. **BDD Tests**: Cucumber tests for CLI workflows
5. **Property Tests**: proptest for protocol invariants

## Build Commands

```bash
# Standard development
cargo make ci-verify      # All CI checks (fmt, clippy, test)
cargo make clippy         # Linter
cargo make test           # All tests

# Platform-specific
cargo make wasm-verify    # WASM checks
cargo make nostd-verify   # no_std checks
cargo make embedded-verify # All embedded targets
cargo make all-targets    # Everything
```
