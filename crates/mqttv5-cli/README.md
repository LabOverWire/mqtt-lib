# mqttv5 - MQTT Command Line Interface

A unified MQTT CLI tool for publishing, subscribing, running a broker, benchmarking, and managing authentication credentials. A single `mqttv5` binary covers every common MQTT workflow with consistent flags across all commands, support for TCP/TLS/WebSocket/QUIC transports, and both v5.0 and v3.1.1 protocol versions.

## Features

- Single binary: pub, sub, broker, bench, acl, passwd, and scram subcommands
- Interactive prompts for missing arguments
- Input validation with error messages and suggestions
- MQTT v5.0 and v3.1.1 protocol support (`--protocol-version 3.1.1`)
- Session management: Clean start, session expiry, and persistence
- Will message support: Last will and testament with delay and QoS options
- Automatic reconnection: Opt-in reconnection with exponential backoff
- Multi-transport: TCP, TLS, WebSocket, and QUIC support
- Environment variables: Every flag configurable via `MQTT5_` prefixed env vars
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
mqttv5 passwd bob -b secretpass passwd.txt

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
mqttv5 scram bob -b secretpass scram.txt

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

The CLI is designed for both interactive exploration and automated scripting. When required arguments are missing, it prompts for them interactively with suggestions and validation. Error messages include contextual hints to guide correction. Long flags have short aliases (`-t` for `--topic`, `-q` for `--qos`), and all MQTT v5.0 features — including properties, reason codes, and subscription options — are accessible from the command line.

### Connection Behavior

By default, the CLI exits immediately when the broker disconnects. This prevents duplicate topic takeover issues when clients reconnect with the same client ID.

Use `--auto-reconnect` to enable automatic reconnection with exponential backoff. When enabled:

- The library handles reconnection automatically
- Subscriptions are restored based on session state
- The client continues running until Ctrl+C or target message count reached

### Environment Variables

Every flag on the `broker`, `pub`, and `sub` subcommands can be set via environment variables using the `MQTT5_` prefix followed by the upper-snake-case flag name:

```
--host               → MQTT5_HOST (pub/sub: broker hostname)
--host               → MQTT5_BIND (broker: bind address)
--tls-cert           → MQTT5_TLS_CERT
--max-clients        → MQTT5_MAX_CLIENTS
--non-interactive    → MQTT5_NON_INTERACTIVE
```

CLI flags take precedence over environment variables, which take precedence over defaults. Broker bind addresses use distinct env var names (`MQTT5_BIND`, `MQTT5_TLS_BIND`, etc.) to avoid collision with the client hostname (`MQTT5_HOST`). Repeatable flags accept comma-separated values from env vars.

```bash
# Configure broker entirely via environment variables
export MQTT5_BIND=0.0.0.0:1883
export MQTT5_ALLOW_ANONYMOUS=true
export MQTT5_STORAGE_BACKEND=memory
mqttv5 broker
```

The Docker image sets `MQTT5_NON_INTERACTIVE=true` by default. Use `--help` on any subcommand to see the env var name for each flag (shown as `[env: MQTT5_...]`).

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

# QUIC transport (quic:// skips certificate verification)
mqttv5 pub --url "quic://broker:14567" -t "test/quic" -m "QUIC message"
mqttv5 sub --url "quic://broker:14567" -t "test/+"

# QUIC transport with certificate verification (quics:// verifies by default)
mqttv5 pub --url "quics://broker:14567" -t "test/quic" -m "Secure QUIC" --ca-cert ca.crt
mqttv5 sub --url "quics://broker:14567" -t "test/+" --ca-cert ca.crt

# Message expiry and topic alias
mqttv5 pub -t "sensors/temp" -m "23.5" --message-expiry-interval 300 --topic-alias 1

# Subscription options (retain handling: 0=send, 1=send if new, 2=don't send)
mqttv5 sub -t "config/#" --retain-handling 2 --retain-as-published
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
