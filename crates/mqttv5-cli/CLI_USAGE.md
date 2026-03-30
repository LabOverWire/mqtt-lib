# mqttv5 CLI Usage Guide

Complete reference for the mqttv5 command-line tool.

## Overview

The `mqttv5` CLI is a single binary that covers every common MQTT workflow: running a broker, publishing messages, subscribing to topics, benchmarking performance, and managing authentication credentials. All commands share a consistent set of connection, TLS, and authentication flags, so the patterns you learn for `pub` carry over to `sub`, `bench`, and beyond.

Global flags:

- `--verbose, -v` - Enable verbose logging (`MQTT5_VERBOSE`)
- `--debug` - Enable debug logging (`MQTT5_DEBUG`)

## Quick Start

Start a broker, publish a message, and subscribe — all in three terminals:

```bash
mqttv5 broker --allow-anonymous

mqttv5 pub -t "hello/world" -m "Hello, MQTT!"

mqttv5 sub -t "hello/#" -v
```

## Environment Variables

Every flag on the `broker`, `pub`, and `sub` subcommands can be set via environment variables. The naming convention is `MQTT5_` prefix + upper-snake-case of the long flag name:

| CLI Flag | Environment Variable | Notes |
| --- | --- | --- |
| `--host` (pub/sub) | `MQTT5_HOST` | Broker hostname to connect to |
| `--host` (broker) | `MQTT5_BIND` | TCP bind address(es) |
| `--tls-host` (broker) | `MQTT5_TLS_BIND` | TLS bind address(es) |
| `--ws-host` (broker) | `MQTT5_WS_BIND` | WebSocket bind address(es) |
| `--tls-cert` | `MQTT5_TLS_CERT` | |
| `--max-clients` | `MQTT5_MAX_CLIENTS` | |
| `--non-interactive` | `MQTT5_NON_INTERACTIVE` | |
| `--otel-endpoint` | `MQTT5_OTEL_ENDPOINT` | |

### Precedence

CLI flag > environment variable > default value.

When `--config` is provided to the broker, the config file takes over and other broker flags are ignored (existing behavior, unchanged).

### Repeatable Flags

Flags that accept multiple values (`--host`, `--tls-host`, `--ws-host`, `--ws-tls-host`, `--quic-host`, `--jwt-role-map`, `--jwt-trusted-role-claim`) use comma-separated values when set via environment variables:

```bash
# These are equivalent:
mqttv5 broker --host 0.0.0.0:1883 --host [::]:1883
MQTT5_BIND="0.0.0.0:1883,[::]:1883" mqttv5 broker
```

On the CLI, repeated `-H` flags still work as before. The env var adds comma-splitting as an alternative.

### Boolean Flags

Boolean flags (`--allow-anonymous`, `--retain`, `--insecure`, etc.) treat any non-empty env var value as `true`:

```bash
MQTT5_ALLOW_ANONYMOUS=true mqttv5 broker
MQTT5_ALLOW_ANONYMOUS=1 mqttv5 broker    # also works
```

### Docker Usage

The Docker image sets `MQTT5_NON_INTERACTIVE=true` by default. All broker configuration can be done via env vars:

```bash
docker run -e MQTT5_BIND=0.0.0.0:1883 \
           -e MQTT5_ALLOW_ANONYMOUS=true \
           -e MQTT5_STORAGE_BACKEND=memory \
           -p 1883:1883 \
           mqttv5 broker
```

### Discovery

Run `--help` on any subcommand to see the env var name for each flag:

```bash
mqttv5 broker --help   # shows [env: MQTT5_HOST=] etc.
mqttv5 pub --help
mqttv5 sub --help
```

## Command Reference

### mqttv5 broker

Start an MQTT v5.0 broker with support for multiple transports, authentication methods, and storage backends. The broker listens on one or more addresses and can serve TCP, TLS, WebSocket, and QUIC simultaneously.

The broker also supports a `generate-config` subcommand to produce a full example configuration file.

```
mqttv5 broker generate-config [--output FILE] [--format json|toml]
```

#### Broker Flags

| Flag | Description | Default |
| --- | --- | --- |
| `--config, -c <FILE>` | Configuration file path (JSON format) | None |
| `--host, -H <ADDR>` | TCP bind address(es), can be specified multiple times | `0.0.0.0:1883`, `[::]:1883` |
| `--max-clients <N>` | Maximum concurrent clients | `10000` |
| `--allow-anonymous` | Allow anonymous connections | None (prompted if no auth configured) |
| `--auth-password-file <FILE>` | Password file for authentication | None |
| `--acl-file <FILE>` | ACL file for authorization | None |
| `--auth-method <METHOD>` | Authentication method: `password`, `scram`, `jwt`, `jwt-federated` | `password` if auth file provided |
| `--scram-file <FILE>` | SCRAM credentials file path | None |
| `--tls-cert <FILE>` | TLS certificate file (PEM format) | None |
| `--tls-key <FILE>` | TLS private key file (PEM format) | None |
| `--tls-ca-cert <FILE>` | TLS CA certificate for client verification | None |
| `--tls-require-client-cert` | Require client certificates (mTLS) | `false` |
| `--tls-host <ADDR>` | TLS bind address(es), can be specified multiple times | `0.0.0.0:8883`, `[::]:8883` |
| `--ws-host <ADDR>` | WebSocket bind address(es), can be specified multiple times | None |
| `--ws-tls-host <ADDR>` | WebSocket TLS bind address(es), can be specified multiple times | None |
| `--ws-path <PATH>` | WebSocket path | `/mqtt` |
| `--quic-host <ADDR>` | QUIC bind address(es), requires TLS cert/key | None |
| `--quic-delivery-strategy <S>` | QUIC server delivery strategy: `control-only`, `per-topic`, `per-publish` | `per-topic` |
| `--quic-early-data` | Enable QUIC 0-RTT early data | `false` |
| `--storage-dir <DIR>` | Storage directory for persistence | `./mqtt_storage` |
| `--storage-backend <TYPE>` | Storage backend: `memory` or `file` | `file` |
| `--no-persistence` | Disable message persistence | `false` |
| `--session-expiry <SECS>` | Default session expiry interval in seconds | `3600` |
| `--max-qos <0\|1\|2>` | Maximum QoS level | `2` |
| `--keep-alive <SECS>` | Server keep-alive time in seconds | None |
| `--response-information <STR>` | Response information sent to clients that request it | None |
| `--no-retain` | Disable retained messages | `false` |
| `--no-wildcards` | Disable wildcard subscriptions | `false` |
| `--non-interactive` | Skip interactive prompts | `false` |

##### Broker JWT Auth Flags

| Flag | Description | Default |
| --- | --- | --- |
| `--jwt-algorithm <ALG>` | JWT algorithm: `hs256`, `rs256`, `es256` | None |
| `--jwt-key-file <FILE>` | JWT secret file (HS256) or public key file (RS256/ES256) | None |
| `--jwt-issuer <ISS>` | JWT required issuer | None |
| `--jwt-audience <AUD>` | JWT required audience | None |
| `--jwt-clock-skew <SECS>` | JWT clock skew tolerance | `60` |
| `--jwt-jwks-uri <URL>` | JWKS endpoint URL for federated JWT auth | None |
| `--jwt-fallback-key <FILE>` | Fallback key file when JWKS is unavailable | None |
| `--jwt-jwks-refresh <SECS>` | JWKS refresh interval | `3600` |
| `--jwt-role-claim <PATH>` | Claim path for extracting roles (e.g., `roles`, `realm_access.roles`) | None |
| `--jwt-role-map <MAP>` | Role mapping `claim_value:mqtt_role`, can be specified multiple times | None |
| `--jwt-default-roles <ROLES>` | Default roles for authenticated JWT users (comma-separated) | None |
| `--jwt-role-merge-mode <MODE>` | Role merge mode: `merge` or `replace` (deprecated, use `--jwt-auth-mode`) | `merge` |
| `--jwt-auth-mode <MODE>` | Federated auth mode: `identity-only`, `claim-binding`, `trusted-roles` | None |
| `--jwt-trusted-role-claim <PATH>` | Trusted role claim paths, can be specified multiple times | None |
| `--jwt-session-scoped-roles` | Whether JWT roles are session-scoped (cleared on disconnect) | None |
| `--jwt-issuer-prefix <PREFIX>` | Custom issuer prefix for user ID namespacing | None |
| `--jwt-config-file <FILE>` | Federated JWT config file (JSON) for multi-issuer setup | None |

##### Broker OpenTelemetry Flags (requires `opentelemetry` feature)

| Flag | Description | Default |
| --- | --- | --- |
| `--otel-endpoint <URL>` | OpenTelemetry OTLP endpoint (e.g., `http://localhost:4317`) | None |
| `--otel-service-name <NAME>` | OpenTelemetry service name | `mqttv5-broker` |
| `--otel-sampling <0.0-1.0>` | OpenTelemetry sampling ratio | `1.0` |

#### Broker Examples

Basic broker:

```bash
mqttv5 broker
```

Broker with TLS:

```bash
mqttv5 broker \
  --tls-cert server.pem \
  --tls-key server-key.pem \
  --tls-host 0.0.0.0:8883
```

Broker with authentication:

```bash
mqttv5 broker \
  --auth-password-file passwords.txt \
  --allow-anonymous=false
```

Broker with authentication and authorization (ACL):

```bash
mqttv5 broker \
  --auth-password-file passwords.txt \
  --acl-file acl.txt \
  --allow-anonymous=false
```

Broker with SCRAM-SHA-256 authentication:

```bash
mqttv5 broker \
  --auth-method scram \
  --scram-file scram_credentials.txt \
  --allow-anonymous=false
```

Broker with JWT authentication:

```bash
mqttv5 broker \
  --auth-method jwt \
  --jwt-algorithm rs256 \
  --jwt-key-file public_key.pem \
  --jwt-issuer "https://auth.example.com" \
  --jwt-audience "mqtt-broker"
```

Broker with federated JWT authentication:

```bash
mqttv5 broker \
  --auth-method jwt-federated \
  --jwt-jwks-uri "https://accounts.google.com/.well-known/jwks" \
  --jwt-fallback-key fallback_key.pem \
  --jwt-issuer "https://accounts.google.com" \
  --jwt-auth-mode claim-binding \
  --jwt-role-claim "roles" \
  --jwt-role-map "admin:admin" \
  --jwt-role-map "viewer:readonly"
```

Broker from config file:

```bash
mqttv5 broker --config broker-config.json
```

Multiple transports:

```bash
mqttv5 broker \
  --host 0.0.0.0:1883 \
  --tls-host 0.0.0.0:8883 \
  --tls-cert server.pem \
  --tls-key server-key.pem \
  --ws-host 0.0.0.0:8080
```

Broker with QUIC transport:

```bash
mqttv5 broker \
  --host 0.0.0.0:1883 \
  --quic-host 0.0.0.0:14567 \
  --tls-cert server.pem \
  --tls-key server-key.pem
```

Broker with OpenTelemetry tracing:

```bash
mqttv5 broker \
  --otel-endpoint http://localhost:4317 \
  --otel-service-name my-mqtt-broker \
  --otel-sampling 1.0
```

Generate example configuration file:

```bash
mqttv5 broker generate-config --output config.json --format json
```

Broker as load balancer (redirects clients to backend brokers):

```bash
mqttv5 broker --config lb-config.json
```

Where `lb-config.json` contains a `load_balancer` section:

```json
{
  "bind_addresses": ["0.0.0.0:1883"],
  "load_balancer": {
    "backends": [
      "mqtt://backend1.example.com:1883",
      "mqtt://backend2.example.com:1883"
    ]
  }
}
```

Clients connecting to the load balancer receive a CONNACK with reason code `UseAnotherServer` (0x9C) and a Server Reference property pointing to one of the backends. The client library automatically follows the redirect (up to 3 hops).

The load balancer only redirects — it does not broker messages. You must run the backend brokers separately:

```bash
mqttv5 broker --host 0.0.0.0:1884 --allow-anonymous

mqttv5 broker --host 0.0.0.0:1885 --allow-anonymous

mqttv5 broker --config lb-config.json
```

Where `lb-config.json` points to the running backends:

```json
{
  "bind_addresses": ["0.0.0.0:1883"],
  "load_balancer": {
    "backends": [
      "mqtt://127.0.0.1:1884",
      "mqtt://127.0.0.1:1885"
    ]
  }
}
```

Publish through a load balancer (automatic redirect):

```bash
mqttv5 pub -t test/topic -m "Hello" \
  --url mqtt://lb.example.com:1883 \
  --non-interactive
```

Subscribe through a load balancer (automatic redirect):

```bash
mqttv5 sub -t test/# \
  --url mqtt://lb.example.com:1883 \
  --non-interactive
```

### mqttv5 pub

Publish an MQTT message to a broker. Supports all transport types (TCP, TLS, WebSocket, QUIC), authentication methods, will messages, scheduled and repeated publishing, and request/response patterns.

#### Pub Flags

| Flag | Description | Default |
| --- | --- | --- |
| `--topic, -t <TOPIC>` | MQTT topic (required) | None |
| `--message, -m <MSG>` | Message payload | None |
| `--file, -f <FILE>` | Read message from file | None |
| `--stdin` | Read message from stdin | `false` |
| `--url, -U <URL>` | Broker URL (mqtt://, mqtts://, ws://, wss://, quic://) | None |
| `--host, -H <HOST>` | Broker hostname | `localhost` |
| `--port, -p <PORT>` | Broker port | `1883` |
| `--qos, -q <0\|1\|2>` | QoS level | `0` |
| `--retain, -r` | Retain message | `false` |
| `--message-expiry-interval <SECS>` | Message expiry interval in seconds | None |
| `--topic-alias <N>` | Topic alias (1-65535) | None |
| `--response-topic <TOPIC>` | Response topic for request/response pattern (MQTT 5.0) | None |
| `--correlation-data <HEX>` | Correlation data for request/response pattern (hex-encoded) | None |
| `--wait-response` | Wait for response after publishing (requires `--response-topic`) | `false` |
| `--timeout <SECS>` | Timeout when waiting for response | `30` |
| `--response-count <N>` | Number of responses to wait for (0 = unlimited until timeout) | `1` |
| `--output-format <FMT>` | Output format for responses: `raw`, `json`, `verbose` | `raw` |
| `--username, -u <USER>` | Authentication username | None |
| `--password, -P <PASS>` | Authentication password | None |
| `--auth-method <METHOD>` | Authentication method: `password`, `scram`, `jwt` | `password` |
| `--jwt-token <TOKEN>` | JWT token for JWT authentication | None |
| `--client-id, -c <ID>` | Client ID | Auto-generated |
| `--no-clean-start` | Resume existing session | `false` |
| `--session-expiry <SECS>` | Session expiry interval in seconds | `0` |
| `--keep-alive, -k <SECS>` | Keep-alive interval | `60` |
| `--protocol-version <VER>` | MQTT protocol version: `3.1.1`, `311`, `4`, `5`, `5.0` | `5` |
| `--will-topic <TOPIC>` | Will message topic | None |
| `--will-message <MSG>` | Will message payload | None |
| `--will-qos <0\|1\|2>` | Will message QoS | `0` |
| `--will-retain` | Will message retain flag | `false` |
| `--will-delay <SECS>` | Will message delay in seconds | None |
| `--cert <FILE>` | TLS client certificate (PEM) | None |
| `--key <FILE>` | TLS client private key (PEM) | None |
| `--ca-cert <FILE>` | TLS CA certificate (PEM) | None |
| `--insecure` | Skip TLS certificate verification | `false` |
| `--auto-reconnect` | Enable automatic reconnection | `false` |
| `--non-interactive` | Skip interactive prompts | `false` |
| `--delay <SECS>` | Delay before publishing | None |
| `--repeat <N>` | Repeat publishing N times (0 = infinite until Ctrl+C) | None |
| `--interval <MS>` | Interval between repeated publishes in ms (requires `--repeat`) | `1000` |
| `--at <TIME>` | Schedule publish at specific time (e.g., `14:30`, `2025-01-15T14:30:00`) | None |
| `--quic-stream-strategy <S>` | QUIC stream strategy: `control-only`, `per-publish`, `per-topic`, `per-subscription` | `control-only` |
| `--quic-flow-headers` | Enable QUIC flow headers for state recovery | `false` |
| `--quic-flow-expire <SECS>` | Flow header expiry interval in seconds | `300` |
| `--quic-max-streams <N>` | Maximum concurrent QUIC streams | None |
| `--quic-datagrams` | Enable QUIC datagrams for unreliable transport | `false` |
| `--quic-connect-timeout <SECS>` | QUIC connection timeout in seconds | `30` |
| `--quic-early-data` | Enable QUIC 0-RTT early data | `false` |

##### Pub Codec Flags (requires `codec` feature)

| Flag | Description | Default |
| --- | --- | --- |
| `--codec <CODEC>` | Compress payload using codec: `gzip`, `deflate` | None |
| `--codec-level <0-9>` | Codec compression level (requires `--codec`) | `6` |
| `--codec-min-size <BYTES>` | Minimum payload size for compression (requires `--codec`) | `128` |

##### Pub OpenTelemetry Flags (requires `opentelemetry` feature)

| Flag | Description | Default |
| --- | --- | --- |
| `--otel-endpoint <URL>` | OpenTelemetry OTLP endpoint | None |
| `--otel-service-name <NAME>` | OpenTelemetry service name | `mqttv5-pub` |
| `--otel-sampling <0.0-1.0>` | OpenTelemetry sampling ratio | `1.0` |

#### Pub Examples

Basic publish:

```bash
mqttv5 pub -t test/topic -m "Hello, MQTT"
```

Publish with QoS 1:

```bash
mqttv5 pub -t sensors/temp -m "22.5" -q 1
```

Retained message:

```bash
mqttv5 pub -t status/online -m "true" -r
```

Publish to TLS broker:

```bash
mqttv5 pub -t test/topic -m "Secure message" \
  --url mqtts://broker.example.com:8883 \
  --ca-cert ca.pem
```

Publish over QUIC:

```bash
mqttv5 pub -t test/topic -m "QUIC message" \
  --url quic://broker.example.com:14567 \
  --ca-cert ca.pem
```

Publish over QUIC with multistream:

```bash
mqttv5 pub -t sensors/data -m '{"temp":25.5}' \
  --url quic://broker.example.com:14567 \
  --ca-cert ca.pem \
  --quic-stream-strategy per-publish \
  --quic-flow-headers \
  -q 1
```

Publish from file:

```bash
mqttv5 pub -t data/payload -f message.json -q 1
```

With will message:

```bash
mqttv5 pub -t test/topic -m "Online" \
  --will-topic test/status \
  --will-message "Offline" \
  --will-retain
```

With message expiry (60 seconds):

```bash
mqttv5 pub -t alerts/fire -m "Building A" --message-expiry-interval 60
```

With topic alias:

```bash
mqttv5 pub -t sensors/room1/temperature -m "22.5" --topic-alias 1
```

Request/response pattern:

```bash
mqttv5 pub -t commands/request -m '{"action":"status"}' \
  --response-topic commands/response \
  --wait-response \
  --timeout 10 \
  -q 1
```

Repeated publishing:

```bash
mqttv5 pub -t sensors/data -m "reading" --repeat 100 --interval 500
```

Scheduled publish:

```bash
mqttv5 pub -t alerts/scheduled -m "wake up" --at 14:30
```

SCRAM authentication:

```bash
mqttv5 pub -t test/topic -m "authenticated" \
  --auth-method scram \
  --username alice \
  --password secret
```

JWT authentication:

```bash
mqttv5 pub -t test/topic -m "jwt message" \
  --auth-method jwt \
  --jwt-token eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9...
```

With codec compression:

```bash
mqttv5 pub -t data/large -f large_payload.json \
  --codec gzip --codec-level 9
```

Publish with OpenTelemetry tracing:

```bash
mqttv5 pub -t test/topic -m "Traced message" \
  --otel-endpoint http://localhost:4317 \
  --otel-service-name my-publisher
```

Using MQTT 3.1.1 protocol:

```bash
mqttv5 pub -t test/topic -m "v3.1.1 message" --protocol-version 3.1.1
```

### mqttv5 sub

Subscribe to one or more MQTT topics and print received messages. The subscriber runs until interrupted (Ctrl+C) or a target message count is reached. Use `--auto-reconnect` for long-running subscribers that should survive broker restarts.

#### Sub Flags

| Flag | Description | Default |
| --- | --- | --- |
| `--topic, -t <TOPIC>` | MQTT topic pattern (required) | None |
| `--url, -U <URL>` | Broker URL (mqtt://, mqtts://, ws://, wss://, quic://) | None |
| `--host, -H <HOST>` | Broker hostname | `localhost` |
| `--port, -p <PORT>` | Broker port | `1883` |
| `--qos, -q <0\|1\|2>` | Subscription QoS level | `0` |
| `--verbose, -v` | Include topic names in output | `false` |
| `--count, -n <N>` | Exit after receiving N messages | `0` (infinite) |
| `--no-local` | Don't receive own published messages | `false` |
| `--retain-handling <0\|1\|2>` | Retain handling: 0=send, 1=send if new, 2=don't send | `0` |
| `--retain-as-published` | Keep original retain flag on delivery | `false` |
| `--subscription-identifier <ID>` | Subscription identifier (1-268435455) | None |
| `--username, -u <USER>` | Authentication username | None |
| `--password, -P <PASS>` | Authentication password | None |
| `--auth-method <METHOD>` | Authentication method: `password`, `scram`, `jwt` | `password` |
| `--jwt-token <TOKEN>` | JWT token for JWT authentication | None |
| `--client-id, -c <ID>` | Client ID | Auto-generated |
| `--no-clean-start` | Resume existing session | `false` |
| `--session-expiry <SECS>` | Session expiry interval in seconds | `0` |
| `--keep-alive, -k <SECS>` | Keep-alive interval | `60` |
| `--protocol-version <VER>` | MQTT protocol version: `3.1.1`, `311`, `4`, `5`, `5.0` | `5` |
| `--will-topic <TOPIC>` | Will message topic | None |
| `--will-message <MSG>` | Will message payload | None |
| `--will-qos <0\|1\|2>` | Will message QoS | `0` |
| `--will-retain` | Will message retain flag | `false` |
| `--will-delay <SECS>` | Will message delay in seconds | None |
| `--cert <FILE>` | TLS client certificate (PEM) | None |
| `--key <FILE>` | TLS client private key (PEM) | None |
| `--ca-cert <FILE>` | TLS CA certificate (PEM) | None |
| `--insecure` | Skip TLS certificate verification | `false` |
| `--auto-reconnect` | Enable automatic reconnection | `false` |
| `--non-interactive` | Skip interactive prompts | `false` |
| `--quic-stream-strategy <S>` | QUIC stream strategy: `control-only`, `per-publish`, `per-topic`, `per-subscription` | `control-only` |
| `--quic-flow-headers` | Enable QUIC flow headers for state recovery | `false` |
| `--quic-flow-expire <SECS>` | Flow header expiry interval in seconds | `300` |
| `--quic-max-streams <N>` | Maximum concurrent QUIC streams | None |
| `--quic-datagrams` | Enable QUIC datagrams for unreliable transport | `false` |
| `--quic-connect-timeout <SECS>` | QUIC connection timeout in seconds | `30` |
| `--quic-early-data` | Enable QUIC 0-RTT early data | `false` |

##### Sub Codec Flags (requires `codec` feature)

| Flag | Description | Default |
| --- | --- | --- |
| `--codec <CODEC>` | Enable codec decoding for incoming messages: `gzip`, `deflate`, `all` | None |

##### Sub OpenTelemetry Flags (requires `opentelemetry` feature)

| Flag | Description | Default |
| --- | --- | --- |
| `--otel-endpoint <URL>` | OpenTelemetry OTLP endpoint | None |
| `--otel-service-name <NAME>` | OpenTelemetry service name | `mqttv5-sub` |
| `--otel-sampling <0.0-1.0>` | OpenTelemetry sampling ratio | `1.0` |

#### Sub Examples

Basic subscribe:

```bash
mqttv5 sub -t test/topic
```

Multiple topics:

```bash
mqttv5 sub -t sensors/# -t status/#
```

Subscribe with QoS 1:

```bash
mqttv5 sub -t important/data -q 1
```

Verbose output:

```bash
mqttv5 sub -t test/# -v
```

No-local subscription:

```bash
mqttv5 sub -t test/topic --no-local
```

Subscribe over QUIC:

```bash
mqttv5 sub -t sensors/# \
  --url quic://broker.example.com:14567 \
  --ca-cert ca.pem \
  -v
```

Subscribe over QUIC with per-subscription streams:

```bash
mqttv5 sub -t sensors/# -t commands/# \
  --url quic://broker.example.com:14567 \
  --ca-cert ca.pem \
  --quic-stream-strategy per-subscription \
  --quic-flow-headers \
  -v
```

Subscription with identifier:

```bash
mqttv5 sub -t sensors/+/temperature --subscription-identifier 42 -v
```

With retain handling (don't send retained messages):

```bash
mqttv5 sub -t config/settings --retain-handling 2 -v
```

With retain-as-published (preserve retain flag):

```bash
mqttv5 sub -t status/# --retain-as-published -v
```

Persistent session:

```bash
mqttv5 sub -t data/# \
  --client-id my-subscriber \
  --no-clean-start \
  --session-expiry 3600 \
  -q 1
```

SCRAM authentication:

```bash
mqttv5 sub -t secure/# \
  --auth-method scram \
  --username alice \
  --password secret \
  -v
```

JWT authentication:

```bash
mqttv5 sub -t protected/# \
  --auth-method jwt \
  --jwt-token eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9... \
  -v
```

With codec decoding:

```bash
mqttv5 sub -t data/compressed -v --codec all
```

Subscribe with OpenTelemetry tracing:

```bash
mqttv5 sub -t test/# \
  --otel-endpoint http://localhost:4317 \
  --otel-service-name my-subscriber \
  -v
```

### mqttv5 bench

Run performance benchmarks against a broker. Four modes are available: **throughput** (sustained message rate), **latency** (p50/p95/p99 round-trip times), **connections** (connection setup rate), and **hol-blocking** (head-of-line blocking measurement with per-topic trace output).

#### Bench Flags

| Flag | Description | Default |
| --- | --- | --- |
| `--mode <MODE>` | Benchmark mode: `throughput`, `latency`, `connections`, `hol-blocking` | `throughput` |
| `--duration <SECS>` | Test duration in seconds | `10` |
| `--warmup <SECS>` | Warmup period in seconds | `2` |
| `--payload-size <BYTES>` | Message payload size in bytes | `64` |
| `--topic, -t <TOPIC>` | Publish topic | `bench/test` |
| `--filter, -f <FILTER>` | Subscription filter (defaults to topic) | Same as topic |
| `--qos, -q <0\|1\|2>` | QoS level | `0` |
| `--url, -U <URL>` | Full broker URL (mqtt://, mqtts://, ws://, wss://, quic://) | None |
| `--host, -H <HOST>` | Broker hostname | `localhost` |
| `--port, -p <PORT>` | Broker port | `1883` |
| `--client-id, -c <ID>` | Client ID prefix | Auto-generated |
| `--publishers <N>` | Number of publisher clients | `1` |
| `--subscribers <N>` | Number of subscriber clients | `1` |
| `--concurrency <N>` | Concurrent connections (connections mode) | `10` |
| `--topics <N>` | Number of topics (hol-blocking mode) | `4` |
| `--rate <N>` | Publish rate in msg/s (0 = unlimited) | `0` |
| `--payload-format <FMT>` | Payload format: `raw`, `json`, `bebytes`, `compressed-json` | `raw` |
| `--trace-dir <DIR>` | Directory for trace CSV output (hol-blocking mode) | None |
| `--insecure` | Skip TLS certificate verification | `false` |
| `--ca-cert <FILE>` | TLS CA certificate (PEM) | None |
| `--cert <FILE>` | TLS client certificate (PEM) | None |
| `--key <FILE>` | TLS client private key (PEM) | None |
| `--pub-url <URL>` | Separate URL for publishers in HOL mode | None |
| `--quic-stream-strategy <S>` | QUIC stream strategy: `control-only`, `per-publish`, `per-topic`, `per-subscription` | `control-only` |
| `--quic-flow-headers` | Enable QUIC flow headers for state tracking | `false` |
| `--quic-flow-expire <SECS>` | Flow header expiry interval in seconds | `300` |
| `--quic-max-streams <N>` | Maximum concurrent QUIC streams | None |
| `--quic-datagrams` | Enable QUIC datagrams for unreliable transport | `false` |
| `--quic-connect-timeout <SECS>` | QUIC connection timeout in seconds | `30` |
| `--quic-early-data` | Enable QUIC 0-RTT early data | `false` |

#### Bench Examples

Throughput benchmark (default):

```bash
mqttv5 bench --duration 15 --subscribers 5
```

Latency benchmark (measures p50/p95/p99):

```bash
mqttv5 bench --mode latency --duration 10
```

Connection rate benchmark:

```bash
mqttv5 bench --mode connections --duration 10 --concurrency 10
```

HOL blocking test:

```bash
mqttv5 bench --mode hol-blocking --topics 4 --rate 500 --duration 60 \
  --trace-dir ./traces
```

HOL blocking with separate publisher transport (TCP pub, QUIC sub):

```bash
mqttv5 bench --mode hol-blocking --topics 4 --rate 500 --duration 60 \
  --url quic://localhost:14567 --pub-url mqtt://localhost:1883 \
  --quic-stream-strategy per-topic --ca-cert ca.pem \
  --trace-dir ./traces
```

Custom payload and QoS:

```bash
mqttv5 bench --payload-size 1024 --qos 1 --publishers 5 --subscribers 5
```

Wildcard subscription testing:

```bash
mqttv5 bench --topic "bench/test" --filter "bench/+"
```

Benchmark over QUIC with per-topic streams:

```bash
mqttv5 bench --mode latency \
  --url quic://broker.example.com:14567 \
  --ca-cert ca.pem \
  --quic-stream-strategy per-topic
```

Benchmark with JSON payload format:

```bash
mqttv5 bench --payload-format json --payload-size 256
```

### mqttv5 passwd

Manage the password file used by the broker's password authentication. Passwords are hashed with Argon2id before storage.

#### Passwd Usage

```
mqttv5 passwd [OPTIONS] <USERNAME> [FILE]
```

#### Passwd Flags

| Flag | Description |
| --- | --- |
| `--create, -c` | Create new password file (overwrites if exists) |
| `--batch, -b <P>` | Password on command line (insecure, use for scripts only) |
| `--delete, -D` | Delete user from password file |
| `--stdout, -n` | Output hash to stdout instead of file |

#### Passwd Examples

Create password file and add user:

```bash
mqttv5 passwd -c alice passwords.txt
```

Add user to existing file:

```bash
mqttv5 passwd bob passwords.txt
```

Delete user:

```bash
mqttv5 passwd -D alice passwords.txt
```

Batch mode (scripting):

```bash
mqttv5 passwd -b "mypassword" charlie passwords.txt
```

Generate hash to stdout:

```bash
mqttv5 passwd -n testuser
```

### mqttv5 scram

Manage SCRAM-SHA-256 credentials for the broker's challenge-response authentication. SCRAM credentials use PBKDF2-HMAC-SHA256 key derivation with a configurable iteration count.

#### Scram Usage

```
mqttv5 scram [OPTIONS] <USERNAME> [FILE]
```

#### Scram Flags

| Flag | Description | Default |
| --- | --- | --- |
| `--create, -c` | Create new SCRAM file (overwrites if exists) | `false` |
| `--batch, -b <P>` | Password on command line (insecure, use for scripts only) | None |
| `--delete, -D` | Delete user from SCRAM file | `false` |
| `--stdout, -n` | Output credentials to stdout instead of file | `false` |
| `--iterations, -i <N>` | PBKDF2 iteration count (minimum 10000) | `310000` |

#### Scram File Format

SCRAM credentials files use one line per user with five colon-separated fields:

```
username:salt:iterations:stored_key:server_key
```

#### Scram Examples

Create SCRAM file and add user:

```bash
mqttv5 scram -c alice scram_credentials.txt
```

Add user to existing file:

```bash
mqttv5 scram bob scram_credentials.txt
```

Delete user:

```bash
mqttv5 scram -D alice scram_credentials.txt
```

Batch mode (scripting):

```bash
mqttv5 scram -b "mypassword" charlie scram_credentials.txt
```

Generate credentials to stdout:

```bash
mqttv5 scram -n testuser
```

Custom iteration count:

```bash
mqttv5 scram -i 500000 alice scram_credentials.txt
```

### mqttv5 acl

Manage ACL (Access Control List) files for the broker's topic-level authorization. ACL rules control which users can publish or subscribe to which topics, with support for wildcards, roles, and username substitution.

#### ACL Usage

```
mqttv5 acl <COMMAND>
```

#### ACL Commands

| Command | Description |
| --- | --- |
| `add <user> <topic> <permission> --file FILE` | Add ACL rule for a user |
| `remove <user> [topic] --file FILE` | Remove ACL rule(s) for user |
| `list [user] --file FILE` | List ACL rules (all or for specific user) |
| `check <user> <topic> <action> --file FILE` | Check if user can perform action on topic |
| `role-add <role> <topic> <permission> --file FILE` | Add ACL rule for a role |
| `role-remove <role> [topic] --file FILE` | Remove ACL rule(s) for a role |
| `role-list [role] --file FILE` | List role definitions (all or specific role) |
| `assign <user> <role> --file FILE` | Assign a role to a user |
| `unassign <user> <role> --file FILE` | Remove a role from a user |
| `user-roles <user> --file FILE` | List roles assigned to a user |

#### Permissions

- `read` - Allow subscribe operations
- `write` - Allow publish operations
- `readwrite` - Allow both subscribe and publish
- `deny` - Explicitly deny access

#### ACL Examples

Add rule allowing Alice to subscribe to sensors:

```bash
mqttv5 acl add alice "sensors/#" read --file acl.txt
```

Add rule allowing Bob to publish to actuators:

```bash
mqttv5 acl add bob "actuators/#" write --file acl.txt
```

Add rule for all users to access public topics:

```bash
mqttv5 acl add "*" "public/#" readwrite --file acl.txt
```

User-scoped rule with `%u` substitution (expands to authenticated username):

```bash
mqttv5 acl add "*" '$DB/u/%u/#' readwrite --file acl.txt
```

Deny access to admin topics:

```bash
mqttv5 acl add "*" "admin/#" deny --file acl.txt
```

List all ACL rules:

```bash
mqttv5 acl list --file acl.txt
```

List rules for specific user:

```bash
mqttv5 acl list alice --file acl.txt
```

Check if user can perform action:

```bash
mqttv5 acl check alice "sensors/temperature" read --file acl.txt
```

Remove specific rule:

```bash
mqttv5 acl remove alice "sensors/#" --file acl.txt
```

Remove all rules for user:

```bash
mqttv5 acl remove alice --file acl.txt
```

Add a role definition:

```bash
mqttv5 acl role-add sensor-reader "sensors/#" read --file acl.txt
```

Assign a role to a user:

```bash
mqttv5 acl assign alice sensor-reader --file acl.txt
```

List roles for a user:

```bash
mqttv5 acl user-roles alice --file acl.txt
```

Remove a role from a user:

```bash
mqttv5 acl unassign alice sensor-reader --file acl.txt
```

List all role definitions:

```bash
mqttv5 acl role-list --file acl.txt
```

## Configuration File Reference

The broker accepts a JSON configuration file via `--config`. This section documents every field and provides ready-to-use examples.

### Complete Configuration Schema

```json
{
  "bind_addresses": ["string"],
  "max_clients": number,
  "session_expiry_interval": duration,
  "max_packet_size": number,
  "topic_alias_maximum": number,
  "retain_available": boolean,
  "maximum_qos": 0 | 1 | 2,
  "wildcard_subscription_available": boolean,
  "subscription_identifier_available": boolean,
  "shared_subscription_available": boolean,
  "server_keep_alive": number | null,
  "response_information": string | null,
  "auth_config": AuthConfig,
  "tls_config": TlsConfig | null,
  "websocket_config": WebSocketConfig | null,
  "websocket_tls_config": WebSocketConfig | null,
  "storage_config": StorageConfig,
  "load_balancer": LoadBalancerConfig | null,
  "bridges": [BridgeConfig]
}
```

### Core Broker Settings

| Field | Type | Description | Default |
| --- | --- | --- | --- |
| `bind_addresses` | `string[]` | TCP listener addresses | `["0.0.0.0:1883", "[::]:1883"]` |
| `max_clients` | `number` | Maximum concurrent client connections | `10000` |
| `session_expiry_interval` | `duration` | Default session expiry for clients | `"1h"` |
| `max_packet_size` | `number` | Maximum MQTT packet size in bytes | `268435456` (256 MB) |
| `topic_alias_maximum` | `number` | Maximum number of topic aliases | `65535` |
| `retain_available` | `boolean` | Enable retained messages | `true` |
| `maximum_qos` | `0\|1\|2` | Maximum QoS level supported | `2` |
| `wildcard_subscription_available` | `boolean` | Enable wildcard subscriptions | `true` |
| `subscription_identifier_available` | `boolean` | Enable subscription identifiers | `true` |
| `shared_subscription_available` | `boolean` | Enable shared subscriptions | `true` |
| `server_keep_alive` | `number\|null` | Override client keep-alive (seconds) | `null` |
| `response_information` | `string\|null` | Response information property | `null` |

### AuthConfig

```json
{
  "allow_anonymous": boolean,
  "password_file": "string" | null,
  "acl_file": "string" | null,
  "auth_method": "None" | "Password" | "ScramSha256",
  "auth_data": string | null
}
```

| Field | Type | Description | Default |
| --- | --- | --- | --- |
| `allow_anonymous` | `boolean` | Allow anonymous connections | `true` |
| `password_file` | `string\|null` | Path to password file | `null` |
| `acl_file` | `string\|null` | Path to ACL file | `null` |
| `auth_method` | `string` | Authentication method | `"None"` |
| `auth_data` | `string\|null` | Additional auth data | `null` |

### TlsConfig

```json
{
  "cert_file": "string",
  "key_file": "string",
  "ca_file": "string" | null,
  "require_client_cert": boolean,
  "bind_addresses": ["string"]
}
```

| Field | Type | Description | Default |
| --- | --- | --- | --- |
| `cert_file` | `string` | TLS certificate file (PEM) | Required |
| `key_file` | `string` | TLS private key file (PEM) | Required |
| `ca_file` | `string\|null` | CA certificate for client verification | `null` |
| `require_client_cert` | `boolean` | Require client certificates (mTLS) | `false` |
| `bind_addresses` | `string[]` | TLS listener addresses | `["0.0.0.0:8883", "[::]:8883"]` |

### WebSocketConfig

```json
{
  "bind_addresses": ["string"],
  "path": "string",
  "subprotocol": "string",
  "use_tls": boolean,
  "allowed_origins": ["string"] | null
}
```

| Field | Type | Description | Default |
| --- | --- | --- | --- |
| `bind_addresses` | `string[]` | WebSocket listener addresses | Required |
| `path` | `string` | WebSocket endpoint path (non-matching paths return 404) | `"/mqtt"` |
| `subprotocol` | `string` | WebSocket subprotocol | `"mqtt"` |
| `use_tls` | `boolean` | Use TLS for WebSocket | `false` |
| `allowed_origins` | `string[]\|null` | Allowed Origin headers for CSWSH prevention (null = all origins allowed) | `null` |

### StorageConfig

```json
{
  "backend": "File" | "Memory",
  "base_dir": "string",
  "cleanup_interval": duration,
  "enable_persistence": boolean
}
```

| Field | Type | Description | Default |
| --- | --- | --- | --- |
| `backend` | `string` | Storage backend type | `"Memory"` |
| `base_dir` | `string` | Base directory for file storage | `"./mqtt_storage"` |
| `cleanup_interval` | `duration` | Cleanup interval for expired data | `"1h"` |
| `enable_persistence` | `boolean` | Enable message persistence | `false` |

### LoadBalancerConfig

```json
{
  "backends": ["string"]
}
```

| Field | Type | Description | Default |
| --- | --- | --- | --- |
| `backends` | `string[]` | Backend broker URLs | Required |

When `load_balancer` is set, the broker acts as a connection redirector. On each CONNECT, it selects a backend using a hash of the client ID (byte-sum modulo backend count) and responds with CONNACK reason code `UseAnotherServer` (0x9C) containing a Server Reference property. The client automatically follows the redirect (up to 3 hops). The same client ID always routes to the same backend.

Backend URL format determines the transport the client uses after redirect:

| Scheme | Transport | Default Port |
| --- | --- | --- |
| `mqtt://host:port` | TCP | 1883 |
| `mqtts://host:port` | TLS | 8883 |
| `quic://host:port` | QUIC | 14567 |

The backend URL scheme must match the transport the client should use for the backend connection. The LB broker always requires at least one TCP `bind_addresses` entry, even for TLS-only or QUIC-only load balancing.

### BridgeConfig

```json
{
  "name": "string",
  "remote_address": "string",
  "client_id": "string",
  "username": "string" | null,
  "password": "string" | null,
  "use_tls": boolean,
  "tls_server_name": "string" | null,
  "ca_file": "string" | null,
  "client_cert_file": "string" | null,
  "client_key_file": "string" | null,
  "insecure": boolean | null,
  "alpn_protocols": ["string"] | null,
  "try_private": boolean,
  "clean_start": boolean,
  "keepalive": number,
  "protocol_version": "mqttv50" | "mqttv311" | "mqttv31",
  "reconnect_delay": duration,
  "initial_reconnect_delay": duration,
  "max_reconnect_delay": duration,
  "backoff_multiplier": number,
  "max_reconnect_attempts": number | null,
  "backup_brokers": ["string"],
  "topics": [TopicMapping]
}
```

| Field | Type | Description | Default |
| --- | --- | --- | --- |
| `name` | `string` | Unique bridge name | Required |
| `remote_address` | `string` | Remote broker address (host:port) | Required |
| `client_id` | `string` | Client ID for bridge connection | `"bridge-{name}"` |
| `username` | `string\|null` | Authentication username | `null` |
| `password` | `string\|null` | Authentication password | `null` |
| `use_tls` | `boolean` | Enable TLS connection | `false` |
| `tls_server_name` | `string\|null` | Override TLS server name for verification | `null` |
| `ca_file` | `string\|null` | CA certificate file for TLS verification | `null` |
| `client_cert_file` | `string\|null` | Client certificate for mTLS | `null` |
| `client_key_file` | `string\|null` | Client private key for mTLS | `null` |
| `insecure` | `boolean\|null` | Disable TLS certificate verification | `false` |
| `alpn_protocols` | `string[]\|null` | ALPN protocols (e.g., `["x-amzn-mqtt-ca"]` for AWS IoT) | `null` |
| `try_private` | `boolean` | Send bridge user property (Mosquitto compatible) | `true` |
| `clean_start` | `boolean` | Start with clean session | `false` |
| `keepalive` | `number` | Keep-alive interval in seconds | `60` |
| `protocol_version` | `string` | MQTT protocol version | `"mqttv50"` |
| `reconnect_delay` | `duration` | Reconnection delay (deprecated) | `"5s"` |
| `initial_reconnect_delay` | `duration` | Initial reconnection delay | `"5s"` |
| `max_reconnect_delay` | `duration` | Maximum reconnection delay | `"5m"` |
| `backoff_multiplier` | `number` | Exponential backoff multiplier | `2.0` |
| `max_reconnect_attempts` | `number\|null` | Max reconnection attempts (null = infinite) | `null` |
| `backup_brokers` | `string[]` | Backup broker addresses for failover | `[]` |
| `topics` | `TopicMapping[]` | Topic mappings for message forwarding | Required |

### TopicMapping

```json
{
  "pattern": "string",
  "direction": "in" | "out" | "both",
  "qos": "AtMostOnce" | "AtLeastOnce" | "ExactlyOnce",
  "local_prefix": "string" | null,
  "remote_prefix": "string" | null
}
```

| Field | Type | Description | Default |
| --- | --- | --- | --- |
| `pattern` | `string` | Topic pattern with MQTT wildcards | Required |
| `direction` | `string` | Message flow direction | Required |
| `qos` | `string` | QoS level for forwarding | Required |
| `local_prefix` | `string\|null` | Prefix to add to local topics | `null` |
| `remote_prefix` | `string\|null` | Prefix to add to remote topics | `null` |

Direction values: `"in"` forwards from remote to local, `"out"` forwards from local to remote, and `"both"` enables bidirectional forwarding.

### Complete Configuration Examples

#### Minimal Configuration

```json
{
  "bind_addresses": ["0.0.0.0:1883"]
}
```

#### Basic Authenticated Broker

```json
{
  "bind_addresses": ["0.0.0.0:1883"],
  "auth_config": {
    "allow_anonymous": false,
    "password_file": "/etc/mqtt/passwords.txt",
    "auth_method": "Password"
  }
}
```

#### TLS Broker

```json
{
  "bind_addresses": ["0.0.0.0:1883"],
  "tls_config": {
    "cert_file": "/etc/mqtt/certs/server.pem",
    "key_file": "/etc/mqtt/certs/server-key.pem",
    "bind_addresses": ["0.0.0.0:8883"]
  }
}
```

#### Load Balancer Broker

```json
{
  "bind_addresses": ["0.0.0.0:1883"],
  "load_balancer": {
    "backends": [
      "mqtt://backend1.example.com:1883",
      "mqtt://backend2.example.com:1883",
      "mqtt://backend3.example.com:1883"
    ]
  }
}
```

#### TLS Load Balancer Broker

```json
{
  "bind_addresses": ["0.0.0.0:1883"],
  "tls_config": {
    "cert_file": "/etc/mqtt/certs/server.pem",
    "key_file": "/etc/mqtt/certs/server-key.pem",
    "bind_addresses": ["0.0.0.0:8883"]
  },
  "load_balancer": {
    "backends": [
      "mqtts://backend1.example.com:8883",
      "mqtts://backend2.example.com:8883"
    ]
  }
}
```

#### QUIC Load Balancer Broker

```json
{
  "bind_addresses": ["0.0.0.0:1883"],
  "quic_config": {
    "cert_file": "/etc/mqtt/certs/server.pem",
    "key_file": "/etc/mqtt/certs/server-key.pem",
    "bind_addresses": ["0.0.0.0:14567"]
  },
  "load_balancer": {
    "backends": [
      "quic://backend1.example.com:14567",
      "quic://backend2.example.com:14567"
    ]
  }
}
```

#### Broker with Bridge

```json
{
  "bind_addresses": ["0.0.0.0:1883"],
  "bridges": [
    {
      "name": "cloud-bridge",
      "remote_address": "broker.cloud.example.com:8883",
      "client_id": "edge-bridge-01",
      "use_tls": true,
      "ca_file": "/etc/mqtt/certs/ca.pem",
      "topics": [
        {
          "pattern": "sensors/#",
          "direction": "out",
          "qos": "AtLeastOnce"
        },
        {
          "pattern": "commands/#",
          "direction": "in",
          "qos": "AtLeastOnce"
        }
      ]
    }
  ]
}
```

#### Full-Featured Broker

```json
{
  "bind_addresses": ["0.0.0.0:1883", "[::]:1883"],
  "max_clients": 5000,
  "session_expiry_interval": "2h",
  "max_packet_size": 134217728,
  "retain_available": true,
  "maximum_qos": 2,
  "wildcard_subscription_available": true,
  "subscription_identifier_available": true,
  "shared_subscription_available": true,
  "auth_config": {
    "allow_anonymous": false,
    "password_file": "/etc/mqtt/passwords.txt",
    "auth_method": "Password"
  },
  "tls_config": {
    "cert_file": "/etc/mqtt/certs/server.pem",
    "key_file": "/etc/mqtt/certs/server-key.pem",
    "ca_file": "/etc/mqtt/certs/ca.pem",
    "require_client_cert": true,
    "bind_addresses": ["0.0.0.0:8883", "[::]:8883"]
  },
  "websocket_config": {
    "bind_addresses": ["0.0.0.0:8080"],
    "path": "/mqtt",
    "subprotocol": "mqtt"
  },
  "storage_config": {
    "backend": "File",
    "base_dir": "/var/lib/mqtt",
    "cleanup_interval": "30m",
    "enable_persistence": true
  },
  "bridges": [
    {
      "name": "aws-iot-bridge",
      "remote_address": "your-endpoint.iot.us-east-1.amazonaws.com:8883",
      "client_id": "my-device",
      "use_tls": true,
      "ca_file": "/etc/mqtt/certs/AmazonRootCA1.pem",
      "client_cert_file": "/etc/mqtt/certs/device-cert.pem",
      "client_key_file": "/etc/mqtt/certs/device-key.pem",
      "alpn_protocols": ["x-amzn-mqtt-ca"],
      "initial_reconnect_delay": "5s",
      "max_reconnect_delay": "5m",
      "backoff_multiplier": 2.0,
      "topics": [
        {
          "pattern": "device/data/#",
          "direction": "out",
          "qos": "AtLeastOnce"
        }
      ]
    }
  ]
}
```

## Special Topics

This section covers file formats, certificate requirements, and bridge behavior that apply across multiple commands.

### Duration Format

Configuration uses humantime format for duration values:

- `"5s"` - 5 seconds
- `"30m"` - 30 minutes
- `"1h"` - 1 hour
- `"2h30m"` - 2 hours 30 minutes
- `"1d"` - 1 day

### Password File Format

Password files use one line per user:

```
username:$argon2id$v=19$m=...
```

- Username followed by colon
- Argon2 hash of password
- Use `mqttv5 passwd` command to manage

### SCRAM Credentials File Format

SCRAM files use one line per user with five colon-separated fields:

```
username:salt:iterations:stored_key:server_key
```

- Use `mqttv5 scram` command to manage

### ACL File Format

ACL files define topic-level access control with one rule per line:

```
user <username> topic <pattern> permission <type>
```

**Format:**
- `<username>` - Username or `*` for wildcard (all users)
- `<pattern>` - Topic pattern with MQTT wildcards (`+` for single level, `#` for multi-level). Use `%u` to substitute the authenticated username.
- `<type>` - Permission: `read`, `write`, `readwrite`, or `deny`

**Role-based ACL rules:**

```
role <rolename> topic <pattern> permission <type>
assign <username> <rolename>
```

**Example ACL file:**

```
user alice topic sensors/# permission read
user bob topic actuators/# permission write
user admin topic admin/# permission readwrite
user * topic public/# permission readwrite
user * topic admin/# permission deny
user * topic $DB/u/%u/# permission readwrite
role sensor-reader topic sensors/# permission read
role actuator-writer topic actuators/# permission write
assign alice sensor-reader
assign bob actuator-writer
```

**Rule Priority:**
- More specific rules override general rules
- User-specific rules take precedence over wildcard rules
- Deny rules have highest priority

**Security:**
- `%u` substitution rejects usernames containing `+`, `#`, or `/` to prevent wildcard injection
- Anonymous clients never match `%u` patterns
- Sessions are bound to authenticated user identity -- reconnecting with a different user is rejected
- On session restore, subscriptions are re-checked against current ACL rules

Use `mqttv5 acl` command to manage ACL files.

### TLS Certificates

Requirements:

- PEM format for all certificates and keys
- Certificate chain in single file (server cert first, intermediates after)
- Private key must be unencrypted
- CA file can contain multiple CA certificates

### Bridge Configuration

**Direction Types:**

- `in` - Receive messages from remote broker
- `out` - Send messages to remote broker
- `both` - Bidirectional message forwarding

**Reconnection Behavior:**

- Exponential backoff: delay = initial_delay \* (multiplier ^ attempt)
- Default: 5s -> 10s -> 20s -> 40s -> ... -> 300s (max)
- Resets to initial delay after successful connection

**Backup Brokers:**

- Tried in order when primary fails
- Each uses same TLS and auth configuration
- Failover is automatic

**Topic Prefixes:**

- `local_prefix` - Added to topics on local broker
- `remote_prefix` - Added to topics on remote broker
- Applied before/after forwarding based on direction
