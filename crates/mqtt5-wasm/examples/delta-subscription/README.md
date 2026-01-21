# Delta Subscription Demo

This example demonstrates the **delta subscription** feature where the broker only delivers messages when the payload differs from the last delivered value to that subscriber.

## What is Delta Subscription?

Delta subscriptions are a broker-configured feature that reduces bandwidth for topics that frequently publish unchanged values (common with sensors). When enabled for specific topic patterns:

1. The broker tracks the last payload hash per topic per subscriber
2. When a new message arrives, it's only delivered if the payload has changed
3. Duplicate payloads are silently dropped, saving bandwidth

## Running the Example

1. Build the WASM package:
   ```bash
   cd crates/mqtt5-wasm
   wasm-bindgen ../target/wasm32-unknown-unknown/release/mqtt5_wasm.wasm --out-dir examples/delta-subscription/pkg --target web
   ```

2. Serve the example:
   ```bash
   cd examples/delta-subscription
   python3 -m http.server 8080
   ```

3. Open http://localhost:8080 in your browser

## How the Demo Works

- The broker is configured with delta subscriptions enabled for the `sensors/#` pattern
- A subscriber subscribes to `sensors/#` (delta mode is automatically enabled)
- A publisher publishes to `sensors/temperature`
- Only messages with changed payloads are delivered to the subscriber

## Try It

1. Click "Publish Same Value" multiple times - notice only the first message is received
2. Click "Publish Different Value" - the new value is delivered immediately
3. Watch the statistics to see how many messages were published vs delivered

## Configuration

Delta subscriptions are configured on the broker:

```javascript
const config = new WasmBrokerConfig();
config.delta_subscription_enabled = true;
config.add_delta_subscription_pattern('sensors/#');
config.add_delta_subscription_pattern('telemetry/+/status');
```

Any subscription that matches a delta pattern will automatically use delta mode.
