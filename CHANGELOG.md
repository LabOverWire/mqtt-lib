# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.15.0] / [mqtt5-protocol 0.6.0] / [mqtt5-wasm 0.6.0] - 2025-12-19

### Performance

- **Throughput: 180k → 540k msg/s** (3x improvement)
  - RwLock→Mutex for client writer, write buffer reuse, batch message processing, rate limit fast path
- **Zero-allocation PUBLISH encoding** - direct buffer writes, no intermediate Vec
- **Read buffer reuse** - stack-allocated header, reusable payload buffer
- **Write-behind session caching** for file storage (2.6k → 10k conn/s)
- **Memory: 1.5MB → 155KB per connection** - reduced channel buffer capacity

### Added

- Benchmark modes: `connections`, `latency`
- Benchmark options: `--storage-backend`, `--filter`

### Fixed

- **QUIC datagram receiving** - broker now processes incoming QUIC datagrams
  - Added `datagram_receive_buffer_size` configuration to enable datagram reception
  - Added datagram reader loop to decode and route MQTT packets received via datagrams
  - Enables ultra-low-latency QoS 0 PUBLISH via `--quic-datagrams` flag

### Changed

- Consolidated transport configuration
- Increased default `max_connections_per_ip`
- **Code quality**: removed clippy allows, added `#[must_use]`, fixed unsafe cast, removed dead code

## [0.14.0] / [mqtt5-protocol 0.5.0] / [mqtt5-wasm 0.5.0] - 2025-12-18

### Added

- **Authentication rate limiting** protects against brute-force attacks
  - Configurable via `RateLimitConfig` in `AuthConfig`
  - Default: 5 attempts per 60 seconds, 5-minute lockout
  - IP-based tracking with automatic cleanup

- **Federated JWT authentication** with RBAC integration
  - Multi-issuer support with automatic JWKS key refresh
  - Three auth modes: `IdentityOnly`, `ClaimBinding`, `TrustedRoles`
  - Session-scoped roles option for enhanced security
  - Role mappings from JWT claims to broker roles

### Changed

- **JWKS HTTP client** now uses hyper instead of manual HTTP parsing
  - Proper HTTP/1.1 compliance with automatic chunked encoding handling
  - Better TLS validation via hyper-rustls

### Security

- **JWT sub claim validation** prevents namespace injection attacks
  - Rejects sub claims containing `:` character
  - Rejects control characters
  - Enforces 256-character maximum length

## [0.13.0] / [mqtt5-protocol 0.4.0] / [mqtt5-wasm 0.4.0] - 2025-12-11

### Fixed

- **Retained message delivery** now always sets retain flag to true
  - Previously incorrectly cleared retain flag based on `retain_as_published` option
  - `retain_as_published` only affects normal message routing, not retained message delivery to new subscribers

- **Max QoS validation** on incoming PUBLISH packets
  - Broker now rejects PUBLISH messages that exceed advertised `maximum_qos`
  - Returns `QoSNotSupported` reason code in PUBACK/PUBREC

- **retain_handling** now passed during session restore
  - Previously lost when client reconnected with `clean_start=false`

### Changed

- **`with_credentials()` password parameter** changed from `impl Into<Vec<u8>>` to `impl AsRef<[u8]>`
  - Enables cleaner API: `.with_credentials("user", "password")` instead of `.with_credentials("user", b"password")`
  - Accepts `&str`, `&[u8]`, `Vec<u8>`, and byte literals

- **Broker config duration fields** now use human-readable format via `humantime_serde`
  - `session_expiry_interval`, `server_keep_alive`, `cleanup_interval`
  - Example: `session_expiry_interval = "1h"` instead of `{ secs = 3600, nanos = 0 }`

- **WASM broker `allow_anonymous`** now defaults to `false`
  - Configure via `WasmBrokerConfig.allow_anonymous = true` to allow anonymous connections
  - Aligns with secure-by-default approach used in native broker

### Added

- **WASM broker ACL support**
  - `add_acl_rule(username, topic_pattern, permission)` - permission: "read", "write", "readwrite", "deny"
  - `clear_acl_rules()` - remove all ACL rules
  - `acl_rule_count()` - get number of configured rules
  - Uses `AclManager::allow_all()` by default (all authenticated users have full access)

- **Auth Tools example** (`examples/auth-tools/`)
  - Browser-based password hash generator using same Argon2 algorithm as CLI
  - ACL rule builder with topic wildcard support
  - Copy or download generated files for use with native broker

## [0.12.0] / [mqtt5-protocol 0.3.0] / [mqtt5-wasm 0.3.0] - 2025-12-07

### Fixed

- **WASM WebSocket** now sends `mqtt` subprotocol per spec [MQTT-6.0.0-3]
- **QUIC stream frame transmission race condition** in `send_packet_on_stream()`
  - Added `tokio::task::yield_now()` after `SendStream::finish()` to allow QUIC I/O driver to transmit frames
  - Fixes issue where rapid sequential publishes could queue streams faster than transmission, causing data loss on disconnect

### Changed

- **Secure-first CLI authentication UX** following Mosquitto 2.0+/EMQX 5.0+ patterns
  - `--allow-anonymous` no longer defaults to true
  - Password file provided without flag → anonymous defaults to false (secure)
  - Non-interactive mode without auth config → clear error with options
  - Interactive mode without auth config → prompts user for decision
  - Explicit `--allow-anonymous` flag works as before
- ACK packet macro refactored to eliminate duplication using helper macros
- bebytes updated from 2.10 to 2.11

### Added

- **MQTT v3.1.1 protocol support** for client, broker, and CLI
  - Full backwards compatibility with MQTT v3.1.1 brokers and clients
  - CLI `--protocol-version` flag accepts `3.1.1`, `v3.1.1`, `4`, `5.0`, `v5.0`, or `5`
  - Broker accepts both v3.1.1 and v5.0 clients simultaneously
  - WASM client supports `protocolVersion` option (4 for v3.1.1, 5 for v5.0)

- **Cross-protocol interoperability** between v3.1.1 and v5.0 clients
  - v3.1.1 clients can publish to v5.0 subscribers and vice versa
  - Messages encoded with subscriber's protocol version (not publisher's)
  - Subscription stores subscriber's protocol version for correct message delivery

- **WASM callback properties** for MQTT5 request-response patterns
  - JavaScript callbacks now receive `(topic, payload, properties)` instead of `(topic, payload)`
  - `WasmMessageProperties` struct exposes: `responseTopic`, `correlationData`, `contentType`, `payloadFormatIndicator`, `messageExpiryInterval`, `subscriptionIdentifiers`, `getUserProperties()`
  - Enables request-response patterns with correlation data echo and dynamic response topics

## [0.11.4] / [mqtt5-protocol 0.2.1] - 2025-12-04

### Fixed

- **QUIC data stream byte consumption bug** in broker's QUIC acceptor
  - `try_read_flow_header()` was consuming bytes when checking for flow headers but not returning them when they weren't flow headers
  - This caused QUIC stream strategies (DataPerPublish, DataPerTopic, DataPerSubscription) to fail with "Malformed packet" errors
  - Added `FlowHeaderResult` with `leftover` field to preserve non-flow-header bytes
  - Added `read_packet_with_buffer()` to use leftover bytes before reading from stream

### Added

- **MQTT v5.0 implementation gaps** addressed
  - Bridge failover with exponential backoff reconnection
  - Quota management for publish rate limiting
  - Receive maximum enforcement in client

## [0.11.3] / [mqtt5-wasm 0.2.4] - 2025-12-03

### Fixed

- **Subscription options now persist across session restore** (both brokers)
  - Added `StoredSubscription` struct to store full MQTT5 subscription options
  - `no_local`, `retain_as_published`, `retain_handling`, and `subscription_id` now preserved
  - Previously hardcoded to defaults on reconnect with `clean_start=false`

- **WASM broker subscription_id** now passed to router during subscribe
  - Was stored in session but not registered with router

- **WASM broker retain_as_published** handling for retained messages
  - Now respects the flag instead of always setting `retain=true`

### Added

- **Integration tests** for subscription options persistence across reconnection

## [mqtt5-wasm 0.2.3] - 2025-12-01

### Fixed

- **WASM broker keep-alive timeout** now properly triggers will message publication
  - Fixed blocked packet read preventing keep-alive detection
  - Uses channel-based signaling to interrupt async reads on timeout

### Added

- **Will message example** demonstrating MQTT Last Will and Testament feature
  - In-tab broker with two clients (observer + device with will)
  - 5-second keep-alive with countdown timer UI
  - Force disconnect button to trigger will message

## [0.11.2] - 2025-11-29

### Changed

- Bump mqtt5-protocol to 0.2.0 to include MQoQ reason codes

## [0.11.1] - 2025-11-28

### Fixed

- **Bridge message loop prevention** for bidirectional bridges
  - Added `route_message_local_only()` method to MessageRouter
  - Native bridge connections now use local-only routing for incoming messages
  - WASM bridge connections updated to prevent message echo loops
  - Prevents infinite loops when bridges forward messages back and forth

- **WASM bridge `no_local` subscription support**
  - Bridge subscriptions now use `no_local=true` to prevent receiving own messages
  - Added `subscribe_with_callback_internal_opts()` for internal bridge use
  - Fixes client echo issue in bidirectional WASM broker bridges

### Added

- **WASM broker-bridge example** with comprehensive documentation
  - Two-broker bidirectional bridge demonstration
  - Visual diagram of bridge architecture
  - Explanation of `no_local` flag importance for bridges
  - Debug logging panel for message flow tracing

- **Broker readiness signal** via `ready_receiver()` method
  - Returns a `watch::Receiver<bool>` that signals when broker is accepting connections
  - Eliminates need for arbitrary sleep delays in tests and applications
  - Used throughout bridge tests for reliable startup synchronization

### Changed

- Cargo.toml keyword changed from "iot" to "client" for crates.io

## [0.11.0] - 2025-11-26

### Added

- **mqtt5-wasm npm package** published at version 0.1.2
  - Available via `npm install mqtt5-wasm`
  - Fixed README with correct installation instructions for both npm and Cargo
  - Package available at https://www.npmjs.com/package/mqtt5-wasm

- **WASM broker bridging** for connecting in-browser brokers via MessagePort
  - `WasmBridgeConfig` for bridge name, client settings, topic mappings
  - `WasmTopicMapping` with pattern, direction (In/Out/Both), QoS, prefixes
  - `add_bridge(config, port)` / `remove_bridge(name)` broker methods
  - Bidirectional message forwarding between brokers

- **Tracing instrumentation** for transport layer
  - `#[instrument]` attributes on transport connect/read/write operations
  - Structured logging for connection lifecycle events
  - Debug-level tracing for packet I/O operations

- **QUIC transport support** for MQTT over QUIC (RFC 9000)
  - QUIC URL scheme: `quic://host:port` (default port 14567)
  - Built-in TLS 1.3 encryption (QUIC mandates encryption)
  - Certificate verification with configurable CA certificates
  - Server name indication (SNI) support
  - ALPN protocol negotiation (`mqtt`)

- **QUIC multistream architecture** for parallel MQTT operations
  - Eliminates head-of-line blocking for concurrent publishes
  - Stream strategies: `ControlOnly`, `DataPerPublish`, `DataPerTopic`, `DataPerSubscription`
  - Configurable stream management with `QuicStreamManager`
  - Automatic stream lifecycle management

- **Flow headers for stream state recovery**
  - `FlowId` - Unique identifier for stream state tracking (client/server initiated)
  - `FlowFlags` - Recovery mode, persistent QoS, subscription state flags
  - `DataFlowHeader` - Stream initialization with expire intervals
  - `FlowRegistry` - Server-side flow state management
  - bebytes-based bit field serialization for compact encoding

- **QUIC client configuration**
  - `QuicClientConfig` builder pattern for connection setup
  - Insecure mode for development/testing
  - CA certificate loading from file or PEM bytes
  - Custom server name verification

- **Receive-side multistream support**
  - Background stream acceptor for server-initiated streams
  - Packet routing from multiple concurrent streams
  - Stream-aware packet decoding with flow context

### Technical Details

- **Transport layer**: Quinn 0.11 for QUIC implementation
- **Crypto provider**: Ring for TLS operations
- **Stream management**: Async stream multiplexing with tokio
- **Flow encoding**: bebytes derive macros for zero-copy serialization
- **Test coverage**: 20 multistream integration tests, unit tests for all components

### Compatibility

- EMQX 5.0+ (native MQTT-over-QUIC support)
- Standard QUIC servers supporting ALPN `mqtt`

## [0.10.0] - 2025-11-24

### BREAKING CHANGES

- **Workspace restructuring**: Project reorganized into proper Rust workspace with three crates
  - **mqtt5-protocol**: Platform-agnostic MQTT v5.0 core (packets, types, Transport trait)
  - **mqtt5**: Native client and broker for Linux, macOS, Windows
  - **mqtt5-wasm**: WebAssembly client and broker for browsers
  - Library moved to `crates/mqtt5/` directory
  - CLI remains in `crates/mqttv5-cli/` as sister project
  - Import paths unchanged for native: `use mqtt5::*` still works
  - WASM now uses: `use mqtt5_wasm::*` and imports from `./pkg/mqtt5_wasm.js`
  - Example paths:
    - Native examples: `crates/mqtt5/examples/`
    - WASM examples: `crates/mqtt5-wasm/examples/`
  - Cargo commands now require `-p` flag: `cargo run -p mqtt5 --example simple_broker`
  - Test certificate paths remain at workspace root: `test_certs/`
  - Git history preserved: all files tracked as renames

### Added

- Broker support for Request Response Information property
- Broker support for Request Problem Information property
- Conditional reason strings in error responses based on client preference
- CLI `--response-information` flag for broker command
- CLI subscribe command: `--retain-handling` and `--retain-as-published` options
- CLI publish command: `--message-expiry-interval` and `--topic-alias` options
- Storage version checking with clear error messages for incompatible formats
  - Automatically creates `.storage_version` file in storage directory
  - Detects version mismatches and provides migration instructions
  - Prevents silent data corruption from format changes

- **mqtt5-protocol crate**: Platform-agnostic MQTT v5.0 core extracted from mqtt5

  - Packet encoding/decoding for all MQTT v5.0 packet types
  - Protocol types (QoS, properties, reason codes)
  - Error types (`MqttError`, `Result`)
  - Topic matching and validation
  - Transport trait for platform-agnostic I/O
  - Minimal dependencies: `bebytes`, `bytes`, `serde`, `thiserror`, `tracing`
  - Shared by both mqtt5 (native) and mqtt5-wasm (browser) crates

- **mqtt5-wasm crate**: Dedicated WebAssembly crate for browser environments

  - Full MQTT v5.0 protocol support compiled to WebAssembly
  - Three connection modes:
    - `connect(url)` - WebSocket connection to external MQTT brokers
    - `connect_message_port(port)` - Direct connection to in-tab broker
    - `connect_broadcast_channel(name)` - Cross-tab messaging via BroadcastChannel API
  - **QoS 0 support**: `publish(topic, payload)` for fire-and-forget messaging
  - **QoS 1 support**: `publish_qos1(topic, payload, callback)` with PUBACK acknowledgment
  - **QoS 2 support**: `publish_qos2(topic, payload, callback)` with full four-way handshake
    - PUBLISH → PUBREC → PUBREL → PUBCOMP flow
    - 10-second timeout for incomplete flows
    - Duplicate detection with 30-second tracking window
    - Status tracking for each QoS 2 message
  - **Subscription management**:
    - `subscribe(topic)` - Subscribe without callback
    - `subscribe_with_callback(topic, callback)` - Subscribe with message handler
    - `unsubscribe(topic)` - Remove subscriptions dynamically
  - **Connection event callbacks**:
    - `on_connect(callback)` - Receive CONNACK with reason code and session_present flag
    - `on_disconnect(callback)` - Notified when connection closes
    - `on_error(callback)` - Error notifications including keepalive timeouts
  - **Automatic keepalive**:
    - Sends PINGREQ every 30 seconds automatically
    - Detects connection timeout after 90 seconds
    - Triggers error and disconnect callbacks on timeout
  - **Connection state management**: `is_connected()`, `disconnect()`

- **WASM broker implementation** for in-browser MQTT broker

  - `WasmBroker` - Complete MQTT broker running in browser tab
  - `create_client_port()` - Create MessagePort for client connections
  - Memory-only storage backend (no file I/O in browser)
  - AllowAllAuthProvider for development/testing
  - Full MQTT v5.0 feature support (QoS, retained messages, subscriptions)
  - Perfect for testing, demos, and offline-capable applications

- **WASM transport layer** with async bridge patterns

  - **WebSocket transport**: `WasmWebSocketTransport` using web_sys::WebSocket
  - **MessagePort transport**: Channel-based IPC for in-tab broker communication
  - **BroadcastChannel transport**: Cross-tab messaging for distributed applications
  - Async bridge converting Rust futures to JavaScript Promises
  - Split reader/writer pattern for concurrent packet I/O

- **Platform-gated dependencies** for WASM compatibility

  - Native CLI dependencies (clap, tokio) excluded from WASM builds
  - Conditional compilation for WASM vs native targets
  - Time module abstraction (std::time vs web_sys::window)
  - Single codebase supporting native and WASM targets

- **WASM examples** demonstrating browser usage in `crates/mqtt5-wasm/examples/`
  - `websocket/` - Connect to external MQTT brokers
  - `qos2/` - QoS 2 flow testing with status visualization
  - `local-broker/` - In-tab broker demonstration
  - Complete browser applications with HTML/JavaScript/CSS
  - Build infrastructure with `wasm-pack` and `build.sh` script

### Enhanced

- BDD test infrastructure updated for workspace structure

  - Dynamic workspace root discovery
  - CLI binary path resolution using CARGO_BIN_EXE environment variable
  - TLS certificate paths computed from workspace root
  - All 30 BDD scenarios passing (140 steps)

- Documentation updated for three-crate architecture
  - ARCHITECTURE.md now documents crate organization and dependencies
  - README.md explains mqtt5-protocol, mqtt5, and mqtt5-wasm separation
  - Cargo commands include `-p` flag for workspace navigation
  - Example paths updated: `crates/mqtt5/examples/` and `crates/mqtt5-wasm/examples/`
  - GitHub Actions workflows updated for new structure
  - Test certificate paths fixed from package-relative to workspace-relative (`../../test_certs/`)
  - Doctests updated to use correct module paths (`mqtt5_protocol::`, `std::time::Duration`)

### Technical Details

- **Crate Organization**:
  - mqtt5-protocol: Platform-agnostic core with minimal dependencies
  - mqtt5: Native implementation depends on mqtt5-protocol
  - mqtt5-wasm: Browser implementation depends on mqtt5-protocol
  - Transport trait abstraction enables platform-specific I/O implementations
  - Consistent MQTT v5.0 compliance across all platforms
- **WASM Architecture**: Single-threaded using Rc<RefCell<T>> instead of Arc<Mutex<T>>
- **WASM Background Tasks**: Using spawn_local (JavaScript event loop) instead of tokio::spawn
- **WASM Packet Encoding**: Full MQTT v5.0 codec running in browser
- **WASM Limitations**: No TLS socket control (use wss://), no file I/O, no raw sockets
- **Workspace Benefits**: Shared metadata, cleaner structure, better IDE support

### Fixed

- Broker now correctly clears retain flag when delivering retained messages to subscribers with `retain_as_published=false`

## [0.9.0] - 2025-11-12

### Added

- **OpenTelemetry distributed tracing support** (behind `opentelemetry` feature flag)
  - W3C trace context propagation via MQTT user properties (`traceparent`, `tracestate`)
  - automatic span creation for broker publish operations
  - automatic span creation for subscriber message reception
  - bridge trace context forwarding to maintain traces across broker boundaries
  - `TelemetryConfig` for OpenTelemetry initialization configuration
  - `BrokerConfig::with_opentelemetry()` method to enable tracing
- new example: `broker_with_opentelemetry.rs` demonstrating distributed tracing setup
- trace context extraction and injection utilities in `telemetry::propagation` module
- `From<MessageProperties> for PublishProperties` conversion for property forwarding

### Enhanced

- bridge connections now forward all MQTT v5 user properties including trace context
- subscriber callbacks receive complete trace context for distributed observability
- client publish operations automatically inject trace context when telemetry is enabled

## [0.8.0] - 2025-11-09

### Added

- subscription identifier CLI support (`--subscription-identifier` flag)
- ACL CLI command (`mqttv5 acl`) for managing ACL files
- authorization debug logging for troubleshooting ACL issues
- `ComprehensiveAuthProvider::with_providers()` constructor for custom auth providers

### Fixed

- **MQTT v5.0 compliance**: client now validates PUBACK/PUBREC/PUBCOMP reason codes
  - returns `MqttError::PublishFailed(reason_code)` when broker rejects publish
  - properly handles ACL authorization failures (NotAuthorized 0x87)
  - fixes issue where client reported success despite broker rejecting publish
- shared subscription callback matching by stripping share prefix during dispatch

## [0.7.0] - 2025-11-05

### Added

- bridge TLS/mTLS support with CA certificates and client certificates
- bridge exponential backoff reconnection (5s → 10s → 20s → 300s max)
- bridge `try_private` option for Mosquitto compatibility
- comprehensive CLI usage guide (CLI_USAGE.md) with full configuration reference

### Changed

- minimum supported Rust version (MSRV) updated to 1.83

### Fixed

- $SYS topic wildcard matching to prevent bridge message loops
- $SYS topic loop warnings in broker logs

## [0.6.0] - 2025-01-25

### Added

- no local subscription option support
- `--no-local` flag to subscribe command

## [0.5.0] - 2025-01-24

### Added

- improved cargo-make workflow with help command
- comprehensive CLI testing suite
- session management options
- will message support for pub and sub commands

### Changed

- updated tokio-tungstenite to 0.28
- updated dialoguer to 0.12

### Fixed

- MaximumQoS property handling per MQTT v5.0 spec
- will message testing timeouts
- repository cleanup and gitignore configuration

## [0.4.1] - 2025-08-05

### Fixed

- **Reduced CLI verbosity** - Changed default log level from WARN to ERROR
- **Fixed logging levels** - Normal operations no longer logged as errors/warnings
  - Task lifecycle messages (starting/exiting) changed from error/warn to debug
  - Connection and DNS resolution messages changed from warn to debug
  - Reconnection monitoring messages changed from warn to info
  - Server disconnect changed from error to info

## [0.4.0] - 2025-08-04

### Added

- **Unified mqttv5 CLI Tool** - Complete MQTT CLI implementation
  - Single binary with pub, sub, and broker subcommands
  - Superior user experience with smart prompting for missing arguments
  - Input validation with helpful error messages and corrections
  - Both long and short flags for improved ergonomics
  - Complete self-reliance - no external MQTT tools needed
- **Complete MQTT v5.0 Broker Implementation**
  - Production-ready broker with full MQTT v5.0 compliance
  - Multi-transport support: TCP, TLS, WebSocket in single binary
  - Built-in authentication: Username/password, file-based, bcrypt
  - Access Control Lists (ACL) for fine-grained topic permissions
  - Broker-to-broker bridging with loop prevention
  - Resource monitoring with connection limits and rate limiting
  - Session persistence and retained message storage
  - Shared subscriptions for load balancing
  - Hot configuration reload without restart
- **Advanced Connection Retry System**
  - Smart error classification distinguishing recoverable from non-recoverable errors
  - AWS IoT-specific error handling (RST, connection limit detection)
  - Exponential backoff with configurable retry policies
  - Different retry strategies for different error types

### Changed

- **Platform Transformation**: Project evolved from client library to complete MQTT v5.0 platform
- **Unified CLI**: All documentation and examples now use mqttv5 CLI
- **Comprehensive Documentation Overhaul**:
  - Restructured docs/ with separate client/ and broker/ sections
  - Added complete broker configuration reference
  - Added authentication and security guides
  - Added deployment and monitoring documentation
  - Updated all examples to show dual-platform usage
- **Development Workflow**: Standardized on cargo-make for consistent CI/build commands
- **Architecture**: Maintained NO EVENT LOOPS principle throughout broker implementation

### Removed

- Unimplemented AuthMethod::External references from documentation

## [0.2.0] - 2025-07-30

### Added

- **Complete MQTT v5.0 protocol implementation** with full compliance
- **BeBytes 2.6.0 integration** for high-performance zero-copy serialization
- **Comprehensive async/await API** with no event loops (pure Rust async patterns)
- **Advanced security features**:
  - TLS/SSL support with certificate validation
  - Mutual TLS (mTLS) authentication support
  - Custom CA certificate support for enterprise environments
- **Production-ready connection management**:
  - Automatic reconnection with exponential backoff
  - Session persistence (clean_start=false support)
  - Client-side message queuing for offline scenarios
  - Flow control respecting broker receive maximum limits
- **Focused library examples**:
  - Simple client and broker examples demonstrating core API
  - Transport examples (TCP, TLS, WebSocket) showing configuration patterns
  - Shared subscription and bridging examples for advanced features
- **Testing infrastructure**:
  - Mock client trait for unit testing
  - Property-based testing with Proptest
  - Integration tests with real MQTT broker
  - Comprehensive benchmark suite
- **Advanced tracing and debugging**:
  - Structured logging with tracing integration
  - Comprehensive instrumentation throughout the codebase
  - Performance monitoring capabilities
- **Developer experience**:
  - AWS IoT SDK compatible API (subscribe returns packet_id + QoS)
  - Callback-based message routing
  - Zero-configuration for common use cases
  - Extensive documentation and examples

### Technical Highlights

- **Zero-copy message handling** using BeBytes derive macros
- **Direct async methods** instead of event loops or actor patterns
- **Comprehensive error handling** with proper error types
- **Thread-safe design** with Arc/RwLock patterns
- **Memory efficient** with bounded queues and cleanup tasks
- **Production tested** with extensive integration test suite

### Performance

- High-throughput message processing with BeBytes serialization
- Efficient memory usage with zero-copy patterns
- Concurrent connection handling
- Optimized packet parsing and generation

### Dependencies

- `bebytes ^2.6.0` - Core serialization framework
- `tokio ^1.46` - Async runtime
- `rustls ^0.23` - TLS implementation
- `bytes ^1.10` - Efficient byte handling
- `thiserror ^2.0` - Error handling
- `tracing ^0.1` - Structured logging

### Examples

Eight focused examples demonstrating library capabilities:

1. **simple_client** - Basic client API usage with callbacks and publishing
2. **simple_broker** - Minimal broker setup and configuration
3. **broker_with_tls** - Secure TLS/SSL transport configuration
4. **broker_with_websocket** - WebSocket transport for browser clients
5. **broker_all_transports** - Multi-transport broker (TCP/TLS/WebSocket)
6. **broker_bridge_demo** - Broker-to-broker bridging configuration
7. **broker_with_monitoring** - $SYS topics and resource monitoring
8. **shared_subscription_demo** - Load balancing with shared subscriptions

## [0.3.0] - 2025-08-01

### Added

- **Certificate loading from bytes**: Load TLS certificates from memory (PEM/DER formats)
  - `load_client_cert_pem_bytes()` - Load client certificates from PEM byte arrays
  - `load_client_key_pem_bytes()` - Load private keys from PEM byte arrays
  - `load_ca_cert_pem_bytes()` - Load CA certificates from PEM byte arrays
  - `load_client_cert_der_bytes()` - Load client certificates from DER byte arrays
  - `load_client_key_der_bytes()` - Load private keys from DER byte arrays
  - `load_ca_cert_der_bytes()` - Load CA certificates from DER byte arrays
- **WebSocket transport support**: Full MQTT over WebSocket implementation
  - WebSocket (ws://) and secure WebSocket (wss://) URL support
  - TLS integration for secure WebSocket connections
  - Custom headers and subprotocol negotiation
  - Client certificate authentication over WebSocket
  - Comprehensive configuration options
- **Property-based testing**: Comprehensive test coverage with Proptest
  - 29 new property-based tests covering edge cases and failure modes
  - Certificate loading robustness testing with arbitrary inputs
  - WebSocket configuration validation across all input domains
  - Memory safety verification for all certificate operations
- **AWS IoT namespace validator**: Topic validation for AWS IoT Core
  - Enforces AWS IoT topic restrictions and length limits (256 chars)
  - Device-specific topic isolation
  - Reserved topic protection
- **Supply chain security**: Enhanced security measures
  - GPG commit signing setup
  - Dependabot configuration
  - Security policy documentation

### Fixed

- Topic validation now correctly follows MQTT v5.0 specification
- AWS IoT namespace uses correct "things" (plural) path
- Subscription management handles duplicate topics correctly (replacement behavior)
- All clippy warnings resolved (65+ uninlined format strings)

### Enhanced

- TLS configuration now supports loading certificates from memory for cloud deployments
- WebSocket configuration supports all TLS features (client auth, custom CA, etc.)
- Comprehensive examples showing certificate loading patterns for different deployment scenarios
- CI pipeline optimized and all tests passing

### Use Cases Enabled

- **Cloud deployments**: Load certificates from Kubernetes secrets, environment variables
- **Browser applications**: MQTT over WebSocket for web-based IoT dashboards
- **Firewall-restricted environments**: WebSocket transport bypasses TCP restrictions
- **Secret management integration**: Load certificates from Vault, AWS Secrets Manager, etc.

---

**Note**: This project was originally created as a showcase for the BeBytes derive macro capabilities,
demonstrating serialization in real-world MQTT applications. It has evolved into
a full-featured MQTT v5.0 platform with both client and broker implementations,
complete with a unified CLI tool.
