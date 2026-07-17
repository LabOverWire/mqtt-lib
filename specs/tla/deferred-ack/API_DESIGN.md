# Deferred Acknowledgement — Rust API derived from the verified model

Status: **design, not yet implemented.** Every obligation below traces to a checked property in
`TLA_DIARY.md`. This is the bridge from the model to code; read the diary first for context.

Scope note: this design also subsumes what issues #108 and #110 asked for, and deliberately
refuses #109's shape. No outreach on those issues until the implementation is complete,
tested, hardened and verified.

---

## 1. Property → API obligation

| # | Model result | API obligation |
|---|---|---|
| 1 | `DeferredAck.tla`: ack is single-shot by construction (`AppAck` requires and removes the slot) | `AckToken::ack(self)` takes **by value**. Double-ack is a compile error, not a runtime check. Token is **not `Clone`**. |
| 2 | `InvNoAckWithoutDelivery` (`ackedIds ⊆ delivered`) | A token can only be **minted at delivery**, carrying its own packet_id. There is no `client.ack(packet_id)` — an unknown/forged id is unrepresentable. |
| 3 | `InvServerWindowBound` / backpressure non-vacuity: withholding acks throttles the server | **Holding a token IS the backpressure.** Backpressure is expressed via the Receive-Maximum window, never via a Rust channel. |
| 4 | `DeferredAckReader.tla`: UNSAFE variant starves the control plane (reachable **and** `<>[]` liveness violation) | Delivery **must never block the reader task**. No consumer-drainable bounded channel / `Stream`. Reuse the existing non-blocking hand-off (`CallbackManager`'s unbounded worker, `callback.rs:60-70`). |
| 5 | `DeferredAckQoS2.tla`: `InvNoDoubleDelivery` fails without the `has_pubrec` guard | **Prerequisite:** fix #112 first (guard dispatch on `has_pubrec`). Deferred ack widens the redelivery window and makes this routine. |
| 6 | `DeferredAckSession.tla`: clean session ⇒ silent message **loss**; persistent ⇒ no loss but **duplicates** | Deferred ack **must be gated on a persistent session**. Refuse to enable it on `clean_start=true` / zero session-expiry. Duplicates are part of the contract. |
| 7 | `DeferredAckToken.tla`: naive `Drop` wedges the window in 2 steps (silent permanent stall) | `AckToken` **must** `impl Drop`, and `Drop` must free the slot (auto-ack). |

---

## 2. The API

```rust
/// Owns one inbound packet_id and its Receive-Maximum window slot for its lifetime.
/// Not Clone: the slot has exactly one owner.
pub struct AckToken { /* packet_id, qos, disarm flag, ack sender */ }

impl AckToken {
    pub fn packet_id(&self) -> u16;
    pub fn qos(&self) -> QoS;

    /// Acknowledge after durable processing. Consumes the token (obligation 1).
    pub fn ack(self);

    /// Free the slot while signalling this message was NOT processed successfully.
    pub fn reject(self, reason: ReasonCode);
}

impl Drop for AckToken {
    /// Obligation 7: a dropped token must never leak its slot.
    fn drop(&mut self) { /* if still armed, emit ack */ }
}
```

Delivery (satisfies #108's raw-packet ask and #110's ack-timing ask in one call):

```rust
pub async fn subscribe_with_ack<F>(
    &self,
    topic_filter: impl Into<String>,
    options: SubscribeOptions,
    callback: F,
) -> Result<(u16, QoS)>
where
    F: Fn(PublishPacket, AckToken) + Send + Sync + 'static;
```

`PublishPacket.payload` is `Bytes`, so a forwarding consumer gets an `Arc` clone rather than
`Message::from`'s `payload.to_vec()` — the zero-copy win #108 asked for, for free.

Enabling:

```rust
ConnectOptions::default()
    .with_clean_start(false)          // required (obligation 6)
    .with_session_expiry_interval(..) // must be non-zero
    .with_receive_maximum(16)         // required, bounded and non-zero (see §3.3)
    .with_deferred_ack(true)
```

---

## 3. Implementation subtleties (each one is a real trap)

### 3.1 `Drop` is sync; emitting a PUBACK is async
`Drop::drop` cannot `.await` the writer. It must **enqueue** the ack on a non-blocking sender
(`mpsc::UnboundedSender<AckRequest>`) drained by a background task that owns the writer. This is
also what keeps obligation 4 intact: the ack path must never block, and must never run on the
reader task. `ack(self)` uses the same enqueue, then disarms so `Drop` does not re-send.

### 3.2 Crash vs graceful drop — the asymmetry is exactly what we want, and it is free
- **Graceful drop** (early return, `?`, panic unwind): `Drop` runs → slot freed → no wedge
  (obligation 7).
- **Process crash**: `Drop` does **not** run → no ack → broker redelivers on session resume →
  at-least-once *processing* preserved (obligation 6).

Deferring the ack is what buys crash-safety; auto-ack-on-Drop is what prevents the wedge. They
do not conflict because a crash never runs `Drop`. Worth stating in the public docs.

### 3.3 Receive Maximum must be explicit and bounded
The window is the backpressure (obligation 3) and it bounds outstanding tokens. Today's default
is 65535, and `0` means *untracked/unbounded* (`flow_control.rs:81-83`). With deferred ack, both
mean "unbounded outstanding tokens" ⇒ memory growth. **Reject `deferred_ack` unless
`receive_maximum` is explicitly set, non-zero, and sane.**

### 3.4 `mem::forget` reintroduces the wedge — and we cannot stop it
Rust does not guarantee `Drop` runs. `std::mem::forget(token)`, or a token captured in a leaked
`Arc` cycle, leaks the slot and silently wedges the subscription (`DeferredAckToken.tla`
`_leak.cfg`). Not preventable in safe Rust; **document it**. This is the one hazard the type
system does not close.

### 3.5 Reconnect with `session_present = 0`
If we asked to resume but the broker reports no session, every outstanding unacked message is
gone (obligation 6, clean-session loss). Outstanding tokens are then stale — acking them is
meaningless. Surface this (error/event), do not silently continue.

### 3.6 The #112 guard's soundness rests on TCP ordering
The `has_pubrec` guard works because PUBREL cannot overtake a DUP PUBLISH on one connection. If
inbound QoS2 state is ever rebuilt across a reconnect, re-check that assumption.

---

## 4. Deliberately NOT provided

- **`inbound_publishes() -> impl Stream` with consumer backpressure (#109 as written).**
  Model-proven to starve the control plane. Not negotiable.
- **`client.ack(packet_id)` / `manual_acks(true)`.** Admits double-ack, unknown-id acks, and
  leaks. The token closes all three.
- **Exactly-once *processing*.** Not on offer (obligation 6): the consumer must be idempotent.
- **Bounded-outstanding-error policy.** Not modelled, not needed for safety once `Drop` frees
  the slot. Would only add earlier detection of a token-hoarding consumer.
- **AMQP-style nack/requeue.** MQTT has no such thing. `reject(reason)` is terminal, not a
  requeue.

---

## 5. Open questions for review

1. **`Drop` = ack-success or ack-with-reason-code?** Auto-ack-success silently claims a message
   was processed when it was abandoned. Acking with a non-success reason frees the slot *and*
   signals intent. MQTT-5 allows PUBACK reason codes, but they are terminal either way (no
   redelivery), so this is about honesty/observability, not semantics. Leaning: reason code +
   a `warn!`.
2. **Gate hard or warn?** Refuse `deferred_ack` on a clean session (a hard error at connect), or
   allow with a loud warning? The model says clean-session deferred ack is *worse than useless*
   — it degrades at-most-once into silent loss. Leaning: hard error.
3. **Per-subscription or per-connection?** `with_deferred_ack(true)` is connection-wide; a
   per-subscription opt-in is finer but complicates the shared inbound window (the window is
   per-connection, not per-subscription). Leaning: connection-wide.
4. **QoS2 deferral point.** Defer the PUBREC (true at-least-once processing, holds the slot for
   the whole handshake) or defer only the PUBCOMP? Stage 3 models PUBREC-at-delivery. Deferring
   PUBREC is the semantically useful one but needs `store_pubrec` to happen only when the PUBREC
   is actually written, or `has_pubrec` will lie.
