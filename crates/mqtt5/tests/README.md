# MQTT v5.0 Client Integration Tests

This directory contains comprehensive integration tests for the MQTT v5.0 client library.

## Test Categories

### 1. Basic Connection Tests (`client_connection.rs`)
- Connection establishment
- Authentication
- Clean session handling
- Protocol version negotiation

### 2. Publishing Tests (`client_publish.rs`)
- QoS 0/1/2 publishing
- Retained messages
- Large payloads
- Concurrent publishing

### 3. Subscription Tests (`client_subscribe.rs`)
- Topic filters and wildcards
- Multiple subscriptions
- Subscription options
- Callback handling

### 4. Complete Flow Tests (`integration_complete_flow.rs`)
- End-to-end MQTT workflows
- Multiple subscriptions with wildcards
- QoS level handling and downgrade
- Session persistence
- Concurrent operations
- Large payload handling

### 5. Reconnection Tests (`integration_reconnection.rs`)
- Automatic reconnection
- Exponential backoff
- Message queuing during disconnection
- Subscription restoration
- Keep-alive timeout detection

### 6. MQTT v5.0 Features (`integration_mqtt5_features.rs`)
- Properties system
- Will messages with delay
- Topic aliases
- Flow control (Receive Maximum)
- Subscription identifiers
- Shared subscriptions
- Maximum packet size
- Reason codes and strings

### 7. Additional Feature Tests
- **Connection Events** (`connection_events.rs`): Event callback system
- **Message Queuing** (`message_queuing.rs`): Offline message handling
- **Error Recovery** (`error_recovery.rs`): Error handling and recovery
- **Retained Messages** (`retained_messages.rs`): Retained message functionality
- **Will Messages** (`will_messages.rs`): Last Will and Testament
- **Enhanced Authentication** (`enhanced_auth.rs`): AUTH packet support
- **Flow Control** (`flow_control.rs`): QoS flow management
- **Topic Aliases** (`topic_aliases.rs`): Bandwidth optimization
- **Packet Size Limits** (`packet_size_limits.rs`): Size constraints

## Running the Tests

### Prerequisites
1. Rust toolchain (stable)
2. Test certificates generated (`./scripts/generate_test_certs.sh`)

### Running All Integration Tests
```bash
cargo test
```

### Running Individual Test Files
```bash
# Run specific test file
cargo test --test integration_complete_flow

# Run with debug output
RUST_LOG=debug cargo test --test integration_complete_flow -- --nocapture
```

### Running Specific Tests
```bash
# Run specific test by name
cargo test --test integration_complete_flow test_complete_mqtt_flow
```

## Test Configuration

Tests use the built-in `TestBroker` which spawns a broker instance programmatically. No external broker required.

## Writing New Tests

When adding new integration tests:

1. Use unique client IDs with `test_client_id()` helper
2. Clean up resources (disconnect clients, clear retained messages)
3. Use appropriate timeouts for async operations
4. Test both success and failure scenarios
5. Verify MQTT v5.0 specific features when applicable

## Troubleshooting

### Common Issues

1. **Port already in use**: Tests use random ports to avoid conflicts
2. **Tests timeout**: Increase timeout values in test code if needed
3. **Certificate errors**: Run `./scripts/generate_test_certs.sh` to regenerate test certificates

### Debug Output

Run tests with debug output:
```bash
RUST_LOG=debug cargo test --test integration_complete_flow -- --nocapture
```