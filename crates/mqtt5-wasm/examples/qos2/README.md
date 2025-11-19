# MQTT QoS 2 Testing Example

This example demonstrates and tests the QoS 2 (ExactlyOnce) delivery guarantee implementation in the WASM MQTT client.

## What is QoS 2?

QoS 2 is the highest quality of service level in MQTT, guaranteeing that messages are delivered exactly once. It uses a four-way handshake:

1. **PUBLISH** - Client sends message to broker
2. **PUBREC** - Broker acknowledges receipt
3. **PUBREL** - Client releases the message
4. **PUBCOMP** - Broker confirms completion

## Features Tested

This example tests:

- **Outgoing QoS 2 messages**: Publishing with QoS 2 and tracking the full handshake flow
- **Incoming QoS 2 messages**: Receiving QoS 2 messages and handling duplicate detection
- **Timeout handling**: 10-second timeout for incomplete QoS 2 flows
- **Flow status tracking**: Visual display of each step in the QoS 2 handshake
- **Duplicate prevention**: Ensures messages are delivered exactly once, even if retransmitted

## Prerequisites

1. Build the WASM package:
   ```bash
   ./crates/mqtt5/examples/wasm/build.sh
   ```

2. Have an MQTT broker running. You can use:
   - HiveMQ public broker: `ws://broker.hivemq.com:8000/mqtt` (default)
   - Mosquitto public test broker: `ws://test.mosquitto.org:8080`
   - Local Mosquitto: `mosquitto -c mosquitto.conf` (with WebSocket support)
   - The local broker example from this project

## Running the Example

1. Start a local web server:
   ```bash
   cd crates/mqtt5/examples/wasm/qos2
   python3 -m http.server 8000
   ```

2. Open your browser to `http://localhost:8000`

3. Configure the broker URL (default: `ws://broker.hivemq.com:8000/mqtt`)

4. Click "Connect"

5. Test QoS 2 publishing:
   - Enter a topic (e.g., `test/qos2/message`)
   - Enter a payload
   - Click "Publish QoS 2"
   - Watch the "QoS 2 Flow Status" panel to see the handshake progress

6. Test QoS 2 receiving:
   - Subscribe to a topic filter (e.g., `test/qos2/#`)
   - Use another MQTT client to publish QoS 2 messages to the topic
   - Verify messages appear in the "Received Messages" panel
   - Check that duplicate messages are filtered out

## What to Observe

### Successful Flow

When a QoS 2 publish succeeds, you should see:
1. "PUBLISH sent - Waiting for PUBREC" (immediate)
2. "PUBCOMP received - Flow completed successfully" (after handshake completes)

The callback receives reason code `0` for success.

### Timeout

If the broker doesn't complete the handshake within 10 seconds:
1. "PUBLISH sent - Waiting for PUBREC" (immediate)
2. "Timeout waiting for PUBCOMP" (after 10 seconds)

The callback receives `"Timeout"` as the error.

### Duplicate Detection

When receiving QoS 2 messages:
- First delivery: Message appears in "Received Messages"
- Duplicate deliveries: Filtered out automatically (PUBREC sent but message not re-delivered)

## Implementation Details

The QoS 2 implementation uses:

- **Shared state machine**: `src/qos2.rs` contains pure state transition functions used by both native and WASM
- **Timeout tracking**: Background task checks for stale flows every 5 seconds
- **Duplicate tracking**: `received_qos2` HashMap tracks delivered messages for 30 seconds
- **Packet ID management**: Wrapping counter (1-65535) for unique message identification

## Troubleshooting

### "Not connected" error
- Check that the broker URL is correct
- Verify the broker is running and accepting WebSocket connections
- Check browser console for detailed error messages

### Timeout errors
- Verify broker supports QoS 2
- Check network connectivity
- Ensure broker is processing messages correctly

### Messages not received
- Verify subscription topic filter matches published topics
- Check that broker is forwarding messages
- Look for errors in browser console

## Browser Console

Enable browser developer tools (F12) to see detailed logging:
- Connection events
- Packet sends/receives
- QoS 2 flow state transitions
- Error messages

All QoS 2 state changes are logged with `console.log` for debugging.
