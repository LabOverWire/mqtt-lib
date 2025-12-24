# Google JWT Auth Example (WASM Client)

Test federated JWT authentication with Google OAuth using the **mqtt5-wasm** client - our Rust MQTT client compiled to WebAssembly.

## What's Different?

This example uses `mqtt5-wasm` instead of MQTT.js:
- Same MQTT v5 protocol implementation as the native Rust client
- Compiled to WebAssembly for browser use
- Demonstrates interoperability between our broker and our WASM client

## Prerequisites

1. A Google Cloud project with OAuth 2.0 credentials
2. [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/) installed:
   ```bash
   curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
   # or
   cargo install wasm-pack
   ```

## Quick Start

```bash
./run.sh
```

The script will:
1. Build the mqtt5-wasm package (first run only)
2. Build the broker (if needed)
3. Start the broker with Google JWT auth
4. Start the HTTP server at http://localhost:8000

## Manual Setup

### 1. Build the WASM Package

```bash
cd crates/mqtt5-wasm
wasm-pack build --target web --out-dir examples/google-jwt-auth/pkg
```

### 2. Start the MQTT Broker

```bash
./target/release/mqttv5 broker \
  --auth-method jwt-federated \
  --jwt-issuer "https://accounts.google.com" \
  --jwt-jwks-uri "https://www.googleapis.com/oauth2/v3/certs" \
  --jwt-fallback-key /tmp/fallback-public.pem \
  --jwt-audience "YOUR_CLIENT_ID.apps.googleusercontent.com" \
  --ws-host 0.0.0.0:8080
```

### 3. Serve the HTML Application

```bash
python3 -m http.server 8000
```

### 4. Test

1. Open http://localhost:8000
2. Sign in with Google
3. Click "Connect with JWT"
4. Subscribe and publish messages

## How It Works

The WASM client uses the same API as the native Rust client:

```javascript
import init, { WasmMqttClient, WasmConnectOptions } from './pkg/mqtt5_wasm.js';

await init();

const client = new WasmMqttClient('my-client-id');

client.on_connect((reasonCode, sessionPresent) => {
    console.log('Connected!', reasonCode);
});

const options = new WasmConnectOptions();
options.authenticationMethod = 'JWT';
options.set_authenticationData(new TextEncoder().encode(jwtToken));

await client.connect_with_options('ws://localhost:8080/mqtt', options);
await client.subscribe_with_callback('test/#', (topic, payload) => {
    console.log('Message:', new TextDecoder().decode(payload));
});
await client.publish('test/hello', new TextEncoder().encode('Hello!'));
```

## Comparison with MQTT.js Example

| Feature | MQTT.js | mqtt5-wasm |
|---------|---------|------------|
| Size | ~50KB | ~200KB |
| Protocol | Third-party implementation | Our Rust implementation |
| API | Callback-based | Async/Promise |
| MQTT v5 | Full support | Full support |

Both examples validate that our broker correctly implements MQTT v5 with enhanced authentication.
