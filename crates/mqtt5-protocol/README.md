# mqtt5-protocol

[![Crates.io](https://img.shields.io/crates/v/mqtt5-protocol.svg)](https://crates.io/crates/mqtt5-protocol)
[![Documentation](https://docs.rs/mqtt5-protocol/badge.svg)](https://docs.rs/mqtt5-protocol)

MQTT v5.0 and v3.1.1 protocol implementation - packets, encoding, and validation.

## Features

- **`no_std` compatible** - Works on embedded systems without heap or standard library
- **Platform-agnostic** - Runs on Linux, macOS, Windows, WASM, and bare-metal embedded targets
- **Zero-copy parsing** - Uses `bytes` crate for efficient buffer management
- **Full MQTT v5.0** - All packet types, properties, and reason codes

## Installation

```toml
[dependencies]
mqtt5-protocol = "0.9"
```

### For Embedded (no_std)

```toml
[dependencies]
mqtt5-protocol = { version = "0.9", default-features = false }
```

### For Single-Core Embedded

For single-core MCUs, configure via `.cargo/config.toml`:

```toml
[target.riscv32imc-unknown-none-elf]
rustflags = ["--cfg", "portable_atomic_unsafe_assume_single_core"]
```

## Supported Targets

| Target | Platform | Notes |
|--------|----------|-------|
| `x86_64-*`, `aarch64-*` | Desktop/Server | Full std support |
| `wasm32-unknown-unknown` | WebAssembly | Browser/Node.js |
| `thumbv7em-none-eabihf` | ARM Cortex-M4F | STM32F4, nRF52, etc. |
| `riscv32imc-unknown-none-elf` | RISC-V | ESP32-C3, BL602, etc. |

## Usage

### Standard (std)

```rust
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::types::ConnectOptions;
use bytes::BytesMut;

let options = ConnectOptions::new("client-id");
let connect = ConnectPacket::new(options);

let mut buf = BytesMut::new();
connect.encode(&mut buf)?;
```

### Embedded (no_std)

```rust
#![no_std]
extern crate alloc;

use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::types::ConnectOptions;
use mqtt5_protocol::time::{set_time_source, update_monotonic_time};
use bytes::BytesMut;

fn init_mqtt() {
    set_time_source(0, 0);
}

fn timer_tick(millis_since_boot: u64) {
    update_monotonic_time(millis_since_boot);
}

fn create_connect_packet() {
    let options = ConnectOptions::new("device-001");
    let connect = ConnectPacket::new(options);

    let mut buf = BytesMut::with_capacity(128);
    connect.encode(&mut buf).unwrap();
}
```

## Embedded Time Provider

On `no_std` targets, time functions (`Instant::now()`, `SystemTime::now()`) require external time sources since there's no OS. The library provides a time provider mechanism:

```rust
use mqtt5_protocol::time::{
    set_time_source,      // Initialize both monotonic and epoch time
    update_monotonic_time, // Update monotonic clock (for Instant)
    update_epoch_time,     // Update wall clock (for SystemTime)
    Instant,
    Duration,
};

// Call from your timer interrupt or main loop
fn update_time(millis_since_boot: u64) {
    update_monotonic_time(millis_since_boot);
}

// Now Instant::now() and elapsed() work correctly
let start = Instant::now();
// ... do work ...
let elapsed = start.elapsed();
```

### Time Provider Functions

| Function | Purpose |
|----------|---------|
| `set_time_source(monotonic, epoch)` | Initialize both time sources at startup |
| `update_monotonic_time(millis)` | Update monotonic clock (call periodically) |
| `update_epoch_time(millis)` | Update wall clock (if epoch time available) |

### Implementation Notes

- Uses `AtomicU32` pairs for 32-bit target compatibility
- Thread-safe with `SeqCst` ordering
- Returns 0/epoch if time source not initialized

## Crate Organization

This crate provides the protocol layer used by:

- **mqtt5** - Native client and broker for Linux, macOS, Windows
- **mqtt5-wasm** - WebAssembly client and broker for browsers

### Module Structure

| Module | Description |
|--------|-------------|
| `packet` | MQTT packet types (CONNECT, PUBLISH, SUBSCRIBE, etc.) |
| `encoding` | Binary encoding/decoding (strings, binary data, variable integers) |
| `protocol::v5` | MQTT v5.0 properties and reason codes |
| `validation` | Topic validation, namespace rules (AWS IoT, etc.) |
| `session` | Session state (flow control, queues, subscriptions) - no_std compatible |
| `time` | Platform-agnostic time types (Duration, Instant, SystemTime) |
| `types` | Core types (QoS, RetainHandling, etc.) |

## Features

| Feature | Description | Default |
|---------|-------------|---------|
| `std` | Standard library support, enables thiserror and tracing | Yes |

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
