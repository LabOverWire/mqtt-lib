# ACL Permission Demo

This example demonstrates **Access Control Lists (ACL)** with role-based access control (RBAC). Users are assigned roles that define read/write permissions on topic patterns, and the broker enforces these rules on every PUBLISH and SUBSCRIBE.

## Running the Example

1. Build the WASM package:
   ```bash
   cd crates/mqtt5-wasm
   wasm-bindgen ../target/wasm32-unknown-unknown/release/mqtt5_wasm.wasm --out-dir examples/acl-permissions/pkg --target web
   ```

2. Serve the example:
   ```bash
   cd examples/acl-permissions
   python3 -m http.server 8080
   ```

3. Open http://localhost:8080 in your browser

## How the Demo Works

- Four users are pre-configured with distinct roles and permissions
- Select a user to connect as that identity
- Test subscribe and publish operations against various topics
- Operations are checked against the ACL rules and shown as ALLOWED or DENIED
- A permission matrix shows all topic/operation combinations for the selected user

## Users and Roles

| User | Role | Topic Pattern | Permission |
|------|------|---------------|------------|
| Alice | admin | `#` | read/write |
| Bob | publisher | `data/#` | write |
| Carol | subscriber | `data/#` | read |
| Dave | guest | `public/#` | read |

## Try It

1. Start as Alice (admin) — all operations succeed on any topic
2. Switch to Bob (publisher) — can publish to `data/sensors` but cannot subscribe
3. Switch to Carol (subscriber) — can subscribe to `data/sensors` but cannot publish
4. Switch to Dave (guest) — limited to reading `public/#` topics only
5. Try operations on topics outside each user's allowed patterns to see denials
