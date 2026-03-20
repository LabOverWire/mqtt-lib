# Complete MQTT Platform

[![Crates.io](https://img.shields.io/crates/v/mqtt5.svg)](https://crates.io/crates/mqtt5)
[![Documentation](https://docs.rs/mqtt5/badge.svg)](https://docs.rs/mqtt5)
[![Rust CI](https://github.com/LabOverWire/mqtt-lib/actions/workflows/rust.yml/badge.svg)](https://github.com/LabOverWire/mqtt-lib/actions)
[![License](https://img.shields.io/crates/l/mqtt5.svg)](https://github.com/LabOverWire/mqtt-lib#license)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/LabOverWire/mqtt-lib)

**MQTT v5.0 and v3.1.1 platform featuring client library and broker implementation**

## Dual Architecture: Client + Broker

| Component       | Use Case                         | Key Features                                              |
| --------------- | -------------------------------- | --------------------------------------------------------- |
| **MQTT Broker** | Run your own MQTT infrastructure | TLS, WebSocket, QUIC, Authentication, Bridging, Monitoring |
| **MQTT Client** | Connect to any MQTT broker       | Cloud compatible, QUIC multistream, Auto-reconnect, Mock testing |
| **Protocol (no_std)** | Embedded IoT devices       | ARM Cortex-M, RISC-V, ESP32, bare-metal compatible |

## Installation

### Library Crate

```toml
[dependencies]
mqtt5 = "0.28"
```

### CLI Tool

```bash
cargo install mqttv5-cli
```

## Crate Organization

The platform is organized into four crates:

- **mqtt5-protocol** - Platform-agnostic MQTT v5.0 core (packets, types, Transport trait). Supports `no_std` for embedded targets.
- **mqtt5** - Native client and broker for Linux, macOS, Windows
- **mqtt5-wasm** - WebAssembly client and broker for browsers
- **mqtt5-conformance** - OASIS specification conformance test suite (247 normative statements)

## Quick Start

### Start an MQTT Broker

```rust
use mqtt5::broker::{BrokerConfig, MqttBroker};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut broker = MqttBroker::bind("0.0.0.0:1883").await?;

    println!("MQTT broker running on port 1883");

    broker.run().await?;
    Ok(())
}
```

### Connect a Client

```rust
use mqtt5::{MqttClient, QoS};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = MqttClient::new("my-device-001");

    // Multiple transport options:
    client.connect("mqtt://localhost:1883").await?;        // TCP
    // client.connect("mqtts://localhost:8883").await?;    // TLS
    // client.connect("ws://localhost:8080/mqtt").await?;  // WebSocket
    // client.connect("quic://localhost:14567").await?;    // QUIC

    // Subscribe with callback
    client.subscribe("sensors/+/data", |msg| {
        println!("{}: {}", msg.topic, String::from_utf8_lossy(&msg.payload));
    }).await?;

    // Publish a message
    client.publish("sensors/temp/data", b"25.5°C").await?;

    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    Ok(())
}
```

## Command Line Interface (mqttv5)

Single binary with pub, sub, broker, bench, passwd, acl, and scram commands. Supports interactive prompts, input validation, and all transport types.

```bash
cargo install mqttv5-cli

mqttv5 broker --host 0.0.0.0:1883
mqttv5 pub --topic "sensors/temperature" --message "23.5"
mqttv5 sub --topic "sensors/+" --verbose
```

See the [CLI Crate Reference](crates/mqttv5-cli/README.md) and [CLI Usage Guide](crates/mqttv5-cli/CLI_USAGE.md) for the full command reference.

## Features at a Glance

### Broker

The broker supports multiple transports (TCP, TLS, WebSocket, QUIC) in a single binary, built-in authentication (password, SCRAM, JWT, federated JWT), resource monitoring, change-only delivery, load balancer mode, broker-to-broker bridging with loop prevention, event hooks, and OpenTelemetry distributed tracing.

See the [mqtt5 Crate Reference](crates/mqtt5/README.md) for detailed broker capabilities and configuration examples.

### Client

The client supports automatic reconnection with exponential backoff, cloud MQTT broker compatibility (AWS IoT, Azure IoT Hub), QUIC multistream with connection migration, mockable client interface for unit testing, and callback-based message handling.

See the [mqtt5 Crate Reference](crates/mqtt5/README.md) for detailed client capabilities, AWS IoT examples, and mock testing patterns.

### QUIC Transport

MQTT over QUIC provides built-in TLS 1.3, multistream support with configurable strategies (ControlOnly, DataPerPublish, DataPerTopic), connection migration for mobile clients, and flow headers for session state recovery.

See the [QUIC Transport Guide](QUIC_IMPLEMENTATION_SPEC.md) for stream strategies, configuration, and migration usage.

### WASM Browser Support

WebAssembly builds for browser environments with three deployment modes: external broker (WebSocket), in-tab broker (MessagePort), and cross-tab communication (BroadcastChannel).

See the [mqtt5-wasm Crate Reference](crates/mqtt5-wasm/README.md) and [WASM Usage Guide](WASM_USAGE.md) for API reference and framework integration.

### Embedded Support (no_std)

The `mqtt5-protocol` crate supports `no_std` environments for embedded systems including ARM Cortex-M, RISC-V, and ESP32 targets with configurable time provider for hardware timers.

See the [mqtt5-protocol Crate Reference](crates/mqtt5-protocol/README.md) for embedded usage and time provider setup.

### Authentication & Security

Four authentication methods (Password, SCRAM-SHA-256, JWT, Federated JWT), role-based access control with topic-level permissions, composite auth provider chaining, session-to-user binding, and comprehensive transport security.

See the [Authentication & Authorization Guide](AUTHENTICATION.md) for configuration details and security hardening.

## Development & Building

### Prerequisites

- Rust 1.88 or later
- cargo-make (`cargo install cargo-make`)

### Quick Setup

```bash
git clone https://github.com/LabOverWire/mqtt-lib.git
cd mqtt-lib

./scripts/install-hooks.sh

cargo make ci-verify
```

### Available Commands

```bash
cargo make build          # Build the project
cargo make test           # Run all tests
cargo make fmt            # Format code
cargo make clippy         # Run linter
cargo make ci-verify      # Run ALL CI checks (must pass before push)
```

## AI Assistance Disclosure

### Tools and Models Used

This project was developed with assistance from **Claude** (Anthropic) via **Claude Code** (CLI agent). Versions used include Claude Opus 4, Claude Sonnet 4, and Claude Sonnet 3.5. AI assistance was applied across the codebase, documentation, and test suites.

### Nature and Scope of Assistance

AI tools were used for:

- **Code generation** — implementing features, modules, and data structures
- **Refactoring** — restructuring existing code for clarity, performance, and correctness
- **Test scaffolding** — writing unit tests, integration tests, property-based tests, and conformance tests
- **Bug fixing** — diagnosing and resolving defects
- **Documentation drafting** — README content, architecture docs, and inline documentation
- **Code review** — identifying security issues, lint violations, and correctness problems

### Human Review and Oversight

All AI-generated and AI-assisted outputs were reviewed, edited, and validated by human authors. Core architectural decisions, design trade-offs, and feature direction were made by the human maintainers. The human authors take full responsibility for the final state of all code, documentation, and tests in this repository.

## License

This project is licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## Publications

- **Evaluating Stream Mapping Strategies for MQTT over QUIC** — Computer Networks (Elsevier), 2026. Defines three stream mapping strategies (control-only, per-topic, per-publish) and evaluates them across five experiments on GCP infrastructure. Experiment data archived at [Zenodo](https://doi.org/10.5281/zenodo.19098820). See [`publications/comnet/`](publications/comnet/) for the paper, experiment scripts, and reproduction guide.

## Documentation

- [Architecture Overview](ARCHITECTURE.md) - System design and principles
- [Authentication & Authorization](AUTHENTICATION.md) - Auth methods, ACL, RBAC, federated JWT, security
- [mqtt5 Crate Reference](crates/mqtt5/README.md) - Native client and broker API
- [mqtt5-wasm Crate Reference](crates/mqtt5-wasm/README.md) - WebAssembly client and broker
- [mqtt5-protocol Crate Reference](crates/mqtt5-protocol/README.md) - Protocol crate and embedded usage
- [CLI Crate Reference](crates/mqttv5-cli/README.md) - CLI tool overview
- [CLI Usage Guide](crates/mqttv5-cli/CLI_USAGE.md) - Complete CLI reference and examples
- [QUIC Transport Guide](QUIC_IMPLEMENTATION_SPEC.md) - QUIC transport strategies and configuration
- [WASM Usage Guide](WASM_USAGE.md) - JavaScript/TypeScript integration
- [Conformance Test Suite](https://github.com/LabOverWire/mqtt-lib/blob/main/crates/mqtt5-conformance/README.md) - MQTT v5.0 OASIS specification conformance
- [API Documentation](https://docs.rs/mqtt5) - API reference
