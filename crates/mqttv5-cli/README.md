# mqttv5 - MQTT Command Line Interface

[![Crates.io](https://img.shields.io/crates/v/mqttv5-cli.svg)](https://crates.io/crates/mqttv5-cli)
[![Downloads](https://img.shields.io/crates/d/mqttv5-cli.svg)](https://crates.io/crates/mqttv5-cli)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](https://github.com/LabOverWire/mqtt-lib#license)

A unified MQTT CLI tool with pub, sub, and broker commands. Supports v5.0 and v3.1.1.

## Features

- Single binary: pub, sub, broker, and bench subcommands
- Interactive prompts for missing arguments
- Input validation with error messages and suggestions
- MQTT v5.0 and v3.1.1 protocol support (`--protocol-version 3.1.1`)
- Session management: Clean start, session expiry, and persistence
- Will message support: Last will and testament with delay and QoS options
- Automatic reconnection: Opt-in reconnection with exponential backoff
- Multi-transport: TCP, TLS, WebSocket, and QUIC support
- Cross-platform: Linux, macOS, and Windows

## Installation

```bash
cargo install mqttv5-cli
```

## Usage

### Publishing Messages

```bash
# Basic publish
mqttv5 pub --topic "sensors/temperature" --message "23.5°C"

# With QoS and retain
mqttv5 pub -t "sensors/temperature" -m "23.5°C" --qos 1 --retain

# Interactive mode (prompts for missing args)
mqttv5 pub
```

### Subscribing to Topics

```bash
# Basic subscribe
mqttv5 sub --topic "sensors/+"

# Verbose mode shows topic names
mqttv5 sub -t "sensors/#" --verbose

# Subscribe for specific message count
mqttv5 sub -t "test/topic" --count 5

# Session persistence and QoS
mqttv5 sub -t "data/#" --qos 2 --no-clean-start --session-expiry 3600

# Auto-reconnect on disconnect (opt-in)
mqttv5 sub -t "sensors/+" --auto-reconnect
```

### Running a Broker

The broker uses secure-first authentication. You must choose an authentication mode:

```bash
# Anonymous access (for development/testing)
mqttv5 broker --allow-anonymous

# Password authentication
mqttv5 broker --auth-password-file passwd.txt

# SCRAM-SHA-256 authentication
mqttv5 broker --auth-method scram --scram-file scram.txt

# JWT authentication
mqttv5 broker --auth-method jwt --jwt-algorithm rs256 \
  --jwt-key-file public.pem --jwt-issuer "https://auth.example.com"

# Federated JWT (Google OAuth)
mqttv5 broker --auth-method jwt-federated \
  --jwt-issuer "https://accounts.google.com" \
  --jwt-jwks-uri "https://www.googleapis.com/oauth2/v3/certs" \
  --jwt-fallback-key fallback.pem \
  --jwt-auth-mode identity-only

# Federated JWT with trusted roles (Keycloak)
mqttv5 broker --auth-method jwt-federated \
  --jwt-issuer "https://keycloak.example.com/realms/mqtt" \
  --jwt-jwks-uri "https://keycloak.example.com/realms/mqtt/protocol/openid-connect/certs" \
  --jwt-fallback-key fallback.pem \
  --jwt-auth-mode trusted-roles \
  --jwt-trusted-role-claim "realm_access.roles"

# With ACL authorization
mqttv5 broker --auth-password-file passwd.txt --acl-file acl.txt

# In-memory storage (no persistence)
mqttv5 broker --allow-anonymous --storage-backend memory
```

### Managing Passwords

```bash
# Create password file with new user
mqttv5 passwd alice passwd.txt

# Batch mode (password on command line)
mqttv5 passwd -b bob secretpass passwd.txt

# Delete user
mqttv5 passwd -D alice passwd.txt
```

### Managing ACL Rules

```bash
# Add user permissions
mqttv5 acl add alice "sensors/#" readwrite -f acl.txt
mqttv5 acl add bob "sensors/temperature" read -f acl.txt

# Define roles
mqttv5 acl role-add admin "#" readwrite -f acl.txt
mqttv5 acl role-add sensors "sensors/#" readwrite -f acl.txt

# Assign roles to users
mqttv5 acl assign alice admin -f acl.txt

# Check permissions
mqttv5 acl check alice "sensors/temp" write -f acl.txt

# List rules
mqttv5 acl list -f acl.txt
mqttv5 acl user-roles alice -f acl.txt
```

### Managing SCRAM Credentials

```bash
# Create SCRAM credential file with new user
mqttv5 scram alice scram.txt

# Batch mode (password on command line)
mqttv5 scram -b bob secretpass scram.txt

# Use SCRAM authentication with broker
mqttv5 broker --auth-method scram --scram-file scram.txt
```

See [Authentication & Authorization Guide](../../AUTHENTICATION.md) for details.

### Benchmarking

```bash
# Throughput benchmark (default mode)
mqttv5 bench --duration 15 --subscribers 5

# Latency benchmark (measures p50/p95/p99)
mqttv5 bench --mode latency --duration 10

# Connection rate benchmark
mqttv5 bench --mode connections --duration 10 --concurrency 10

# Custom payload and QoS
mqttv5 bench --payload-size 1024 --qos 1 --publishers 5 --subscribers 5

# Wildcard subscription testing
mqttv5 bench --topic "bench/test" --filter "bench/+"
```

## CLI Design

- Error messages with suggestions
- Prompts for missing required arguments
- Single binary for all MQTT operations
- Long flags with short aliases
- MQTT v5.0 support including properties and reason codes

### Connection Behavior

By default, the CLI exits immediately when the broker disconnects. This prevents duplicate topic takeover issues when clients reconnect with the same client ID.

Use `--auto-reconnect` to enable automatic reconnection with exponential backoff. When enabled:

- The library handles reconnection automatically
- Subscriptions are restored based on session state
- The client continues running until Ctrl+C or target message count reached

## Examples

### Publishing sensor data

```bash
mqttv5 pub -t "home/living-room/temperature" -m "22.5" --qos 1
```

### Monitoring all home sensors

```bash
mqttv5 sub -t "home/+/+" --verbose
```

### Testing with retained messages

```bash
mqttv5 pub -t "config/device1" -m '{"enabled": true}' --retain
```

### Advanced MQTT v5.0 Features

```bash
# Will messages for device monitoring
mqttv5 pub -t "sensors/data" -m "active" \
  --will-topic "sensors/status" --will-message "offline" --will-delay 5

# Authentication and session management
mqttv5 sub -t "secure/data" --username user1 --password secret \
  --no-clean-start --session-expiry 7200

# Custom keep-alive and transport options
mqttv5 pub -t "test/topic" -m "data" --keep-alive 120 \
  --url "mqtts://secure-broker:8883"

# WebSocket transport
mqttv5 pub --url "ws://broker:8080/mqtt" -t "test/websocket" -m "WebSocket message"
mqttv5 sub --url "wss://secure-broker:8443/mqtt" -t "test/+"

# QUIC transport (insecure mode for testing)
mqttv5 pub --url "quic://broker:14567" -t "test/quic" -m "QUIC message"
mqttv5 sub --url "quic://broker:14567" -t "test/+"

# QUIC transport with certificate verification
mqttv5 pub --url "quics://broker:14567" -t "test/quic" -m "Secure QUIC" --ca-cert ca.crt
mqttv5 sub --url "quics://broker:14567" -t "test/+" --insecure

# Message expiry and topic alias
mqttv5 pub -t "sensors/temp" -m "23.5" --message-expiry-interval 300 --topic-alias 1

# Subscription options (retain handling: 0=send, 1=send if new, 2=don't send)
mqttv5 sub -t "config/#" --retain-handling 2 --retain-as-published
```

## Environment Variables

- `MQTT_HOST`: Default broker host (default: localhost)
- `MQTT_PORT`: Default broker port (default: 1883)

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
