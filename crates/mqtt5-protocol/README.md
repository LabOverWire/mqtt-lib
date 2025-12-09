# mqtt5-protocol

MQTT v5.0 and v3.1.1 protocol implementation - packets, encoding, and validation.

## Usage

```rust
use mqtt5_protocol::packet::*;
use mqtt5_protocol::types::*;

let connect = ConnectPacket::new("client-id");
let bytes = connect.encode()?;

let packet = Packet::decode(&bytes)?;
```

This crate is used by `mqtt5` and `mqtt5-wasm` for their client and broker implementations.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
