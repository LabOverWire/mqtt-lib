# mqttv5 CLI Usage Guide

Complete reference for the mqttv5 command-line tool.

## Overview

The `mqttv5` CLI provides commands for:

- **broker** - Run an MQTT v5.0 broker
- **pub** - Publish MQTT messages
- **sub** - Subscribe to MQTT topics
- **passwd** - Manage password file for authentication

Global flags:

- `--verbose, -v` - Enable verbose logging
- `--debug` - Enable debug logging

## Command Reference

### mqttv5 broker

Start an MQTT v5.0 broker.

#### Broker Flags

| Flag                          | Description                                                     | Default                     |
| ----------------------------- | --------------------------------------------------------------- | --------------------------- |
| `--config, -c <FILE>`         | Configuration file path (JSON format)                           | None                        |
| `--host, -H <ADDR>`           | TCP bind address(es), can be specified multiple times           | `0.0.0.0:1883`, `[::]:1883` |
| `--max-clients <N>`           | Maximum concurrent clients                                      | `10000`                     |
| `--allow-anonymous`           | Allow anonymous connections                                     | `true`                      |
| `--auth-password-file <FILE>` | Password file for authentication                                | None                        |
| `--acl-file <FILE>`           | ACL file for authorization                                      | None                        |
| `--tls-cert <FILE>`           | TLS certificate file (PEM format)                               | None                        |
| `--tls-key <FILE>`            | TLS private key file (PEM format)                               | None                        |
| `--tls-ca-cert <FILE>`        | TLS CA certificate for client verification                      | None                        |
| `--tls-require-client-cert`   | Require client certificates (mTLS)                              | `false`                     |
| `--tls-host <ADDR>`           | TLS bind address(es), can be specified multiple times           | `0.0.0.0:8883`, `[::]:8883` |
| `--ws-host <ADDR>`            | WebSocket bind address(es), can be specified multiple times     | None                        |
| `--ws-tls-host <ADDR>`        | WebSocket TLS bind address(es), can be specified multiple times | None                        |
| `--ws-path <PATH>`            | WebSocket path                                                  | `/mqtt`                     |
| `--storage-dir <DIR>`         | Storage directory for persistence                               | `./mqtt_storage`            |
| `--no-persistence`            | Disable message persistence                                     | `false`                     |
| `--session-expiry <SECS>`     | Default session expiry interval in seconds                      | `3600`                      |
| `--max-qos <0\|1\|2>`         | Maximum QoS level                                               | `2`                         |
| `--keep-alive <SECS>`         | Server keep-alive time in seconds                               | None                        |
| `--no-retain`                 | Disable retained messages                                       | `false`                     |
| `--no-wildcards`              | Disable wildcard subscriptions                                  | `false`                     |
| `--non-interactive`           | Skip interactive prompts                                        | `false`                     |
| `--otel-endpoint <URL>`       | OpenTelemetry OTLP endpoint (e.g., http://localhost:4317)       | None                        |
| `--otel-service-name <NAME>`  | OpenTelemetry service name                                      | `mqttv5-broker`             |
| `--otel-sampling <0.0-1.0>`   | OpenTelemetry sampling ratio                                    | `1.0`                       |

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

Broker with OpenTelemetry tracing:

```bash
mqttv5 broker \
  --otel-endpoint http://localhost:4317 \
  --otel-service-name my-mqtt-broker \
  --otel-sampling 1.0
```

### mqttv5 pub

Publish an MQTT message.

#### Pub Flags

| Flag                      | Description                                   | Default        |
| ------------------------- | --------------------------------------------- | -------------- |
| `--topic, -t <TOPIC>`     | MQTT topic (required)                         | None           |
| `--message, -m <MSG>`     | Message payload                               | None           |
| `--file, -f <FILE>`       | Read message from file                        | None           |
| `--stdin`                 | Read message from stdin                       | `false`        |
| `--url, -U <URL>`         | Broker URL (mqtt://, mqtts://, ws://, wss://) | None           |
| `--host, -H <HOST>`       | Broker hostname                               | `localhost`    |
| `--port, -p <PORT>`       | Broker port                                   | `1883`         |
| `--qos, -q <0\|1\|2>`     | QoS level                                     | `0`            |
| `--retain, -r`            | Retain message                                | `false`        |
| `--username, -u <USER>`   | Authentication username                       | None           |
| `--password, -P <PASS>`   | Authentication password                       | None           |
| `--client-id, -c <ID>`    | Client ID                                     | Auto-generated |
| `--no-clean-start`        | Resume existing session                       | `false`        |
| `--session-expiry <SECS>` | Session expiry interval in seconds            | `0`            |
| `--keep-alive, -k <SECS>` | Keep-alive interval                           | `60`           |
| `--will-topic <TOPIC>`    | Will message topic                            | None           |
| `--will-message <MSG>`    | Will message payload                          | None           |
| `--will-qos <0\|1\|2>`    | Will message QoS                              | `0`            |
| `--will-retain`           | Will message retain flag                      | `false`        |
| `--will-delay <SECS>`     | Will message delay in seconds                 | `0`            |
| `--cert <FILE>`           | TLS client certificate (PEM)                  | None           |
| `--key <FILE>`            | TLS client private key (PEM)                  | None           |
| `--ca-cert <FILE>`        | TLS CA certificate (PEM)                      | None           |
| `--insecure`              | Skip TLS certificate verification             | `false`        |
| `--auto-reconnect`        | Enable automatic reconnection                 | `false`        |
| `--non-interactive`       | Skip interactive prompts                      | `false`        |
| `--otel-endpoint <URL>`   | OpenTelemetry OTLP endpoint                   | None           |
| `--otel-service-name <NAME>` | OpenTelemetry service name                 | `mqttv5-pub`   |
| `--otel-sampling <0.0-1.0>` | OpenTelemetry sampling ratio                | `1.0`          |

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

Publish with OpenTelemetry tracing:

```bash
mqttv5 pub -t test/topic -m "Traced message" \
  --otel-endpoint http://localhost:4317 \
  --otel-service-name my-publisher
```

### mqttv5 sub

Subscribe to MQTT topics.

#### Sub Flags

| Flag                      | Description                                         | Default        |
| ------------------------- | --------------------------------------------------- | -------------- |
| `--topic, -t <TOPIC>`     | MQTT topic pattern (required, can specify multiple) | None           |
| `--url, -U <URL>`         | Broker URL (mqtt://, mqtts://, ws://, wss://)       | None           |
| `--host, -H <HOST>`       | Broker hostname                                     | `localhost`    |
| `--port, -p <PORT>`       | Broker port                                         | `1883`         |
| `--qos, -q <0\|1\|2>`     | Subscription QoS level                              | `0`            |
| `--verbose, -v`           | Include topic names in output                       | `false`        |
| `--count, -n <N>`         | Exit after receiving N messages                     | `0` (infinite) |
| `--no-local`              | Don't receive own published messages                | `false`        |
| `--subscription-identifier <ID>` | Subscription identifier (1-268435455)         | None           |
| `--username, -u <USER>`   | Authentication username                             | None           |
| `--password, -P <PASS>`   | Authentication password                             | None           |
| `--client-id, -c <ID>`    | Client ID                                           | Auto-generated |
| `--no-clean-start`        | Resume existing session                             | `false`        |
| `--session-expiry <SECS>` | Session expiry interval in seconds                  | `0`            |
| `--keep-alive, -k <SECS>` | Keep-alive interval                                 | `60`           |
| `--will-topic <TOPIC>`    | Will message topic                                  | None           |
| `--will-message <MSG>`    | Will message payload                                | None           |
| `--will-qos <0\|1\|2>`    | Will message QoS                                    | `0`            |
| `--will-retain`           | Will message retain flag                            | `false`        |
| `--will-delay <SECS>`     | Will message delay in seconds                       | `0`            |
| `--cert <FILE>`           | TLS client certificate (PEM)                        | None           |
| `--key <FILE>`            | TLS client private key (PEM)                        | None           |
| `--ca-cert <FILE>`        | TLS CA certificate (PEM)                            | None           |
| `--insecure`              | Skip TLS certificate verification                   | `false`        |
| `--auto-reconnect`        | Enable automatic reconnection                       | `false`        |
| `--non-interactive`       | Skip interactive prompts                            | `false`        |
| `--otel-endpoint <URL>`   | OpenTelemetry OTLP endpoint                         | None           |
| `--otel-service-name <NAME>` | OpenTelemetry service name                       | `mqttv5-sub`   |
| `--otel-sampling <0.0-1.0>` | OpenTelemetry sampling ratio                      | `1.0`          |

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

Subscription with identifier:

```bash
mqttv5 sub -t sensors/+/temperature --subscription-identifier 42 -v
```

Persistent session:

```bash
mqttv5 sub -t data/# \
  --client-id my-subscriber \
  --no-clean-start \
  --session-expiry 3600 \
  -q 1
```

Subscribe with OpenTelemetry tracing:

```bash
mqttv5 sub -t test/# \
  --otel-endpoint http://localhost:4317 \
  --otel-service-name my-subscriber \
  -v
```

### mqttv5 passwd

Manage password file for broker authentication.

#### Passwd Usage

```
mqttv5 passwd [OPTIONS] <USERNAME> [FILE]
```

#### Passwd Flags

| Flag              | Description                                               |
| ----------------- | --------------------------------------------------------- |
| `--create, -c`    | Create new password file                                  |
| `--batch, -b <P>` | Password on command line (insecure, use for scripts only) |
| `--delete, -D`    | Delete user from password file                            |
| `--stdout, -n`    | Output hash to stdout instead of file                     |

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
echo "mypassword" | mqttv5 passwd -b charlie passwords.txt
```

Generate hash to stdout:

```bash
mqttv5 passwd -n testuser
```

### mqttv5 acl

Manage ACL (Access Control List) file for broker authorization.

#### ACL Usage

```
mqttv5 acl <COMMAND>
```

#### ACL Commands

| Command                                       | Description                                  |
| --------------------------------------------- | -------------------------------------------- |
| `add <user> <topic> <permission> --file FILE` | Add ACL rule                                 |
| `remove <user> [topic] --file FILE`           | Remove ACL rule(s) for user                  |
| `list [user] --file FILE`                     | List ACL rules (all or for specific user)    |
| `check <user> <topic> <action> --file FILE`   | Check if user can perform action on topic    |

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

## Configuration File Reference

The broker accepts a JSON configuration file with `--config` flag.

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
  "bridges": [BridgeConfig]
}
```

### Core Broker Settings

| Field                               | Type           | Description                           | Default                         |
| ----------------------------------- | -------------- | ------------------------------------- | ------------------------------- |
| `bind_addresses`                    | `string[]`     | TCP listener addresses                | `["0.0.0.0:1883", "[::]:1883"]` |
| `max_clients`                       | `number`       | Maximum concurrent client connections | `10000`                         |
| `session_expiry_interval`           | `duration`     | Default session expiry for clients    | `"1h"`                          |
| `max_packet_size`                   | `number`       | Maximum MQTT packet size in bytes     | `268435456` (256 MB)            |
| `topic_alias_maximum`               | `number`       | Maximum number of topic aliases       | `65535`                         |
| `retain_available`                  | `boolean`      | Enable retained messages              | `true`                          |
| `maximum_qos`                       | `0\|1\|2`      | Maximum QoS level supported           | `2`                             |
| `wildcard_subscription_available`   | `boolean`      | Enable wildcard subscriptions         | `true`                          |
| `subscription_identifier_available` | `boolean`      | Enable subscription identifiers       | `true`                          |
| `shared_subscription_available`     | `boolean`      | Enable shared subscriptions           | `true`                          |
| `server_keep_alive`                 | `number\|null` | Override client keep-alive (seconds)  | `null`                          |
| `response_information`              | `string\|null` | Response information property         | `null`                          |

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

| Field             | Type           | Description                 | Default  |
| ----------------- | -------------- | --------------------------- | -------- |
| `allow_anonymous` | `boolean`      | Allow anonymous connections | `true`   |
| `password_file`   | `string\|null` | Path to password file       | `null`   |
| `acl_file`        | `string\|null` | Path to ACL file            | `null`   |
| `auth_method`     | `string`       | Authentication method       | `"None"` |
| `auth_data`       | `string\|null` | Additional auth data        | `null`   |

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

| Field                 | Type           | Description                            | Default                         |
| --------------------- | -------------- | -------------------------------------- | ------------------------------- |
| `cert_file`           | `string`       | TLS certificate file (PEM)             | Required                        |
| `key_file`            | `string`       | TLS private key file (PEM)             | Required                        |
| `ca_file`             | `string\|null` | CA certificate for client verification | `null`                          |
| `require_client_cert` | `boolean`      | Require client certificates (mTLS)     | `false`                         |
| `bind_addresses`      | `string[]`     | TLS listener addresses                 | `["0.0.0.0:8883", "[::]:8883"]` |

### WebSocketConfig

```json
{
  "bind_addresses": ["string"],
  "path": "string",
  "subprotocol": "string",
  "use_tls": boolean
}
```

| Field            | Type       | Description                  | Default   |
| ---------------- | ---------- | ---------------------------- | --------- |
| `bind_addresses` | `string[]` | WebSocket listener addresses | Required  |
| `path`           | `string`   | WebSocket endpoint path      | `"/mqtt"` |
| `subprotocol`    | `string`   | WebSocket subprotocol        | `"mqtt"`  |
| `use_tls`        | `boolean`  | Use TLS for WebSocket        | `false`   |

### StorageConfig

```json
{
  "backend": "File" | "Memory",
  "base_dir": "string",
  "cleanup_interval": duration,
  "enable_persistence": boolean
}
```

| Field                | Type       | Description                       | Default            |
| -------------------- | ---------- | --------------------------------- | ------------------ |
| `backend`            | `string`   | Storage backend type              | `"Memory"`         |
| `base_dir`           | `string`   | Base directory for file storage   | `"./mqtt_storage"` |
| `cleanup_interval`   | `duration` | Cleanup interval for expired data | `"1h"`             |
| `enable_persistence` | `boolean`  | Enable message persistence        | `false`            |

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

| Field                     | Type             | Description                                             | Default           |
| ------------------------- | ---------------- | ------------------------------------------------------- | ----------------- |
| `name`                    | `string`         | Unique bridge name                                      | Required          |
| `remote_address`          | `string`         | Remote broker address (host:port)                       | Required          |
| `client_id`               | `string`         | Client ID for bridge connection                         | `"bridge-{name}"` |
| `username`                | `string\|null`   | Authentication username                                 | `null`            |
| `password`                | `string\|null`   | Authentication password                                 | `null`            |
| `use_tls`                 | `boolean`        | Enable TLS connection                                   | `false`           |
| `tls_server_name`         | `string\|null`   | Override TLS server name for verification               | `null`            |
| `ca_file`                 | `string\|null`   | CA certificate file for TLS verification                | `null`            |
| `client_cert_file`        | `string\|null`   | Client certificate for mTLS                             | `null`            |
| `client_key_file`         | `string\|null`   | Client private key for mTLS                             | `null`            |
| `insecure`                | `boolean\|null`  | Disable TLS certificate verification                    | `false`           |
| `alpn_protocols`          | `string[]\|null` | ALPN protocols (e.g., `["x-amzn-mqtt-ca"]` for AWS IoT) | `null`            |
| `try_private`             | `boolean`        | Send bridge user property (Mosquitto compatible)        | `true`            |
| `clean_start`             | `boolean`        | Start with clean session                                | `false`           |
| `keepalive`               | `number`         | Keep-alive interval in seconds                          | `60`              |
| `protocol_version`        | `string`         | MQTT protocol version                                   | `"mqttv50"`       |
| `reconnect_delay`         | `duration`       | Reconnection delay (deprecated)                         | `"5s"`            |
| `initial_reconnect_delay` | `duration`       | Initial reconnection delay                              | `"5s"`            |
| `max_reconnect_delay`     | `duration`       | Maximum reconnection delay                              | `"5m"`            |
| `backoff_multiplier`      | `number`         | Exponential backoff multiplier                          | `2.0`             |
| `max_reconnect_attempts`  | `number\|null`   | Max reconnection attempts (null = infinite)             | `null`            |
| `backup_brokers`          | `string[]`       | Backup broker addresses for failover                    | `[]`              |
| `topics`                  | `TopicMapping[]` | Topic mappings for message forwarding                   | Required          |

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

| Field           | Type           | Description                       | Default  |
| --------------- | -------------- | --------------------------------- | -------- |
| `pattern`       | `string`       | Topic pattern with MQTT wildcards | Required |
| `direction`     | `string`       | Message flow direction            | Required |
| `qos`           | `string`       | QoS level for forwarding          | Required |
| `local_prefix`  | `string\|null` | Prefix to add to local topics     | `null`   |
| `remote_prefix` | `string\|null` | Prefix to add to remote topics    | `null`   |

**Direction values:**

- `"in"` - Forward from remote broker to local broker
- `"out"` - Forward from local broker to remote broker
- `"both"` - Bidirectional forwarding

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
username:$2b$12$hash...
```

- Username followed by colon
- Bcrypt hash of password
- Use `mqttv5 passwd` command to manage

### ACL File Format

ACL files define topic-level access control with one rule per line:

```
user <username> topic <pattern> permission <type>
```

**Format:**
- `<username>` - Username or `*` for wildcard (all users)
- `<pattern>` - Topic pattern with MQTT wildcards (`+` for single level, `#` for multi-level)
- `<type>` - Permission: `read`, `write`, `readwrite`, or `deny`

**Example ACL file:**

```
user alice topic sensors/# permission read
user bob topic actuators/# permission write
user admin topic admin/# permission readwrite
user * topic public/# permission readwrite
user * topic admin/# permission deny
```

**Rule Priority:**
- More specific rules override general rules
- User-specific rules take precedence over wildcard rules
- Deny rules have highest priority

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
- Default: 5s → 10s → 20s → 40s → ... → 300s (max)
- Resets to initial delay after successful connection

**Backup Brokers:**

- Tried in order when primary fails
- Each uses same TLS and auth configuration
- Failover is automatic

**Topic Prefixes:**

- `local_prefix` - Added to topics on local broker
- `remote_prefix` - Added to topics on remote broker
- Applied before/after forwarding based on direction
