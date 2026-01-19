# Codec Compression Example

Demonstrates automatic payload compression using the `codec` feature of `mqtt5-wasm`.

## Features

- **Gzip compression** using pako.js (JavaScript library for reliable WASM compression)
- **Deflate compression** as an alternative codec
- **Automatic encoding** on publish when payload exceeds threshold
- **Automatic decoding** on receive based on `content-type` property (uses miniz_oxide)
- **Compression statistics** showing savings in real-time
- **Fallback to store mode** if pako is not available (valid gzip but no compression)

## Requirements

Include the pako library in your HTML for compression support:

```html
<script src="https://cdnjs.cloudflare.com/ajax/libs/pako/2.1.0/pako.min.js"></script>
```

Without pako, messages will be wrapped in valid gzip format but without compression.

## Building

Build the WASM package with the `codec` feature enabled:

```bash
cd crates/mqtt5-wasm
wasm-pack build --target web --features codec --out-dir examples/codec-compression/pkg
```

## Running

Serve the example with any static file server:

```bash
cd examples/codec-compression
python3 -m http.server 8080
```

Then open http://localhost:8080 in your browser.

## How It Works

1. **Configure Codec**: Select gzip or deflate, set compression level (1-10), and minimum size threshold
2. **Connect**: The codec registry is attached to the connection options
3. **Publish**: Messages larger than the threshold are automatically compressed
4. **Receive**: Messages with `application/gzip` or `application/deflate` content-type are automatically decompressed

## API Usage

```html
<!-- Include pako for compression (required) -->
<script src="https://cdnjs.cloudflare.com/ajax/libs/pako/2.1.0/pako.min.js"></script>
```

```javascript
import {
    WasmMqttClient,
    WasmConnectOptions,
    createCodecRegistry,
    createGzipCodec
} from './pkg/mqtt5_wasm.js';

// Create and configure codec registry
const registry = createCodecRegistry();
const gzipCodec = createGzipCodec(6, 128);  // level 6, 128 byte threshold
registry.registerGzip(gzipCodec);
registry.setDefault('application/gzip');

// Attach to connection options
const options = new WasmConnectOptions();
options.setCodecRegistry(registry);

// Connect - compression happens automatically
const client = new WasmMqttClient('my-client');
await client.connect_with_options('ws://broker:8080/mqtt', options);

// Publish - large payloads are compressed automatically
await client.publish_with_options('topic', largePayload, pubOpts);
```

## Compression Efficiency

Compression works best on:
- JSON data
- Log messages
- Repetitive text
- Structured data

Typical compression ratios:
- JSON: 60-80% size reduction
- Plain text: 40-60% size reduction
- Already compressed data: No benefit (may increase size)
