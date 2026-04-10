# mqtt5-conformance-macros

Proc-macro crate providing the `#[conformance_test]` attribute for the MQTT v5.0 conformance test suite.

## Usage

```rust
use mqtt5_conformance_macros::conformance_test;
use mqtt5_conformance::sut::SutHandle;

#[conformance_test(
    ids = ["MQTT-3.1.0-1"],
    requires = ["transport.tcp"],
)]
async fn connect_first_packet_must_be_connect(sut: SutHandle) {
    // test body runs against any SUT that supports TCP
}
```

## Parameters

| Parameter | Required | Description |
|-----------|----------|-------------|
| `ids` | Yes | Array of MQTT conformance statement IDs. Each must start with `MQTT-` (e.g., `"MQTT-3.1.0-1"`). At least one ID is required. |
| `requires` | No | Array of capability requirement strings. Tests are skipped when the SUT lacks a declared requirement. Defaults to empty (no requirements). |

## Requirement Strings

The `requires` parameter accepts these capability strings:

| String | Description |
|--------|-------------|
| `transport.tcp` | SUT supports TCP connections |
| `transport.tls` | SUT supports TLS connections |
| `transport.websocket` | SUT supports WebSocket connections |
| `transport.quic` | SUT supports QUIC connections |
| `max_qos>=0` | SUT supports QoS 0 (always true) |
| `max_qos>=1` | SUT supports QoS 1 or higher |
| `max_qos>=2` | SUT supports QoS 2 |
| `retain_available` | SUT supports retained messages |
| `wildcard_subscription_available` | SUT supports wildcard topic filters |
| `subscription_identifier_available` | SUT supports subscription identifiers |
| `shared_subscription_available` | SUT supports shared subscriptions |
| `enhanced_auth.<METHOD>` | SUT supports a specific enhanced auth method (e.g., `enhanced_auth.SCRAM-SHA-256`) |
| `hooks.restart` | SUT can be restarted between tests |
| `hooks.cleanup` | SUT state can be cleaned up between tests |
| `acl` | SUT supports topic-level access control |

All requirement strings are validated at compile time. Invalid strings produce a compilation error.

## Function Signature

The annotated function must be:

- **`async`** — synchronous functions are rejected at compile time
- **Exactly one parameter**: `sut: SutHandle`
- **Return type**: `()` (implicit unit return)

```rust
// Correct
async fn my_test(sut: SutHandle) { ... }

// Compile errors:
fn not_async(sut: SutHandle) { ... }           // must be async
async fn too_many(sut: SutHandle, x: u8) { ... } // exactly one parameter
async fn wrong_return(sut: SutHandle) -> bool { ... } // must return ()
```

## Generated Output

The macro generates three items from each annotated function:

1. **Implementation function** — the user's async body, renamed with a `__conformance_impl_` prefix.

2. **`#[tokio::test]` wrapper** — compiled under `#[cfg(test)]`, creates a fresh in-process SUT and calls the implementation. This allows `cargo test` to run conformance tests without an external broker.

3. **Registry entry** — a static `ConformanceTest` struct registered in a `linkme::distributed_slice`. The CLI runner iterates over all entries, evaluates capability requirements against the target SUT, and runs or skips each test accordingly. No runtime registration step is needed.
