# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.10.0] - 2025-11-18

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
  - Native CLI dependencies (clap, tokio, bcrypt) excluded from WASM builds
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
