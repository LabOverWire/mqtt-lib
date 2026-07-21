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

## 5. Resolved decisions

Decided 2026-07-17. Each was resolved to its leaning.

1. **`Drop` emits an ack with a non-success reason code and a `warn!`.** Auto-ack-success would
   silently claim a message was processed when it was abandoned. The reason-coded ack frees the
   slot *and* records intent. Both outcomes are terminal (MQTT-5 PUBACK/PUBCOMP reason codes do
   not trigger redelivery), so this is honesty/observability, not semantics.
2. **Clean session is a hard error at connect.** `deferred_ack` with `clean_start=true` or a
   zero session-expiry is rejected before connecting. Clean-session deferred ack degrades
   at-most-once into silent loss (`DeferredAckSession.tla`); it is unrepresentable at runtime.
3. **Opt-in is connection-wide.** `with_deferred_ack(true)` on `ConnectOptions` applies to every
   subscription. This matches the per-connection Receive-Maximum window; a per-subscription
   opt-in would fragment slot accounting across one shared window.
4. **QoS2 defers the PUBREC.** The token withholds PUBREC and holds the slot for the whole
   handshake — true at-least-once *processing*. See §3.7 for the dedup consequence.

## 3.7 Deferring PUBREC decouples the dedup guard from PUBREC emission

The #112 fix (landed, `handlers.rs`) sends PUBREC at delivery and uses one `inbound_pubrecs`
entry to mean both "already delivered to the app" (dedup) and "PUBREC owed" (handshake). Decision
5.4 splits those two meanings apart, because PUBREC is now emitted at `ack()` time, not at
delivery:

- **At first inbound PUBLISH id `n`:** mark `n` delivered, hand the app a token, **do not send
  PUBREC**. The dedup guard is set here, exactly as today.
- **At `token.ack()`:** send a success PUBREC, record `pubrec-sent`.
- **At `token.reject(reason)` / Drop:** send an **error** PUBREC (reason ≥ 0x80). This is
  **terminal** — MQTT-5 discards the exchange, no PUBREL follows — so it must **not** set
  `pubrec-sent`, and it frees the slot.
- **At PUBREL:** clear `pubrec-sent`, send PUBCOMP.

So the guard needs two bits per inbound id — *delivered* and *pubrec-sent* — where today
`mark_pubrec_pending` collapses them into one. `has_pubrec` (used by `handle_pubrel`) must key off
*pubrec-sent*. **Model reference: `DeferredAckQoS2Reconnect.tla`** (`InvDeferralHeld`,
`InvNoPubrecOnWireBeforeResolve`, `InvDoneImpliesNoPubrec`).

**Where DUPs actually come from (corrected by the Stage 5 quorum — this is load-bearing).** MQTT-5
`[MQTT-4.4.0-1]` forbids resending on a live connection; this repo enforces it
(`no_spontaneous_retransmission_on_active_connection`). The broker resends an unacked QoS2 PUBLISH
with `dup=true` **only on session-resume reconnect** (`resend_inflight_messages`,
`broker/client_handler/publish.rs`, gated by `session_present` in `connect.rs`). Consequences:

- **The backpressure is the Receive-Maximum slot, not a retransmit timer.** A held (unresolved)
  token keeps its id outstanding in the broker's window; once the window fills, the broker stops
  sending *new* messages. Withholding PUBREC produces silence on a live link, not a stream of DUPs.
  The slot is taken at `register_inbound_publish` and freed at PUBREL (`acknowledge_inbound`) —
  deferral **widens how long the slot is held**; it does **not** move the free point to `ack()`.
- **A DUP only ever arrives after a reconnect**, and only for an id the broker never got a PUBREC
  for (i.e. an unresolved or resolve-lost token). On that DUP: suppress delivery (guard), and
  re-send the ack **matching the recorded decision** — success if acked, the **same error** if
  rejected, and **nothing** if still unresolved. Re-sending a fabricated success PUBREC for a
  rejected id would resurrect it (a real trap the quorum caught).
- **What the reconnect guarantees depends on WHAT the client keeps — and the dedup guard and the
  token must be kept together.** Two independent pieces of client state can be lost on a resume: the
  `SessionState` dedup bits (`delivered`/`resolution`/`pubrecSent`, in-memory `inbound_pubrecs`
  today) and the app's in-memory `AckToken`. `DeferredAckQoS2Reconnect.tla` models both axes and
  proves three regimes:
  - **Transport reconnect** (both survive in memory): exactly-once DELIVERY holds — the replayed
    PUBLISH is suppressed and the still-held token resolves on the new connection. (`_transport`
    config, `InvNoDoubleDelivery` holds.)
  - **Process crash** (both lost — `inbound_pubrecs` is not persisted): the replay re-delivers and
    re-mints a token → at-least-once PROCESSING, the documented contract (decision 5.2 / Stage 4a).
    Double delivery is expected here and NO wedge occurs. (`_crash` config, `InvNoWedge` holds,
    `InvNoDoubleDelivery` intentionally not asserted.)
  - **The forbidden middle** — persisting `delivered` to disk while the token is lost on crash:
    the replay is suppressed (guard survives) yet no token is minted, so the message can NEVER be
    resolved and the broker's Receive-Maximum slot wedges permanently. (`_persistmistake` config,
    `InvNoWedge` VIOLATED.)
  **Design rule this pins:** do NOT persist the dedup guard `delivered` unless you ALSO restore the
  resolve capability on the replayed message (re-mint a token on a DUP of a delivered-but-unresolved
  id). The simplest safe implementation keeps inbound QoS2 dedup state in memory only — it survives a
  transport reconnect and is lost together with the token on a crash, landing in the two safe
  regimes and never the wedge. This corrects the earlier "`delivered` MUST survive the reconnect"
  note, which was wrong: unconditional survival is exactly the wedge.

---

## 6. Implementation status and remaining follow-ups

**Implemented (branch `deferred-ack-token`, commit `implement deferred-ack AckToken and subscribe_with_ack`):**
the `AckToken` (move-only; `ack`/`reject` consume; `Drop` auto-acks reason-coded + `warn!`), the
connection-stable `AckDispatcher` with a swappable writer slot, the two-bit dedup split in
`SessionState` (`inbound_delivered` vs `inbound_pubrecs`, in-memory only), the `ConnectOptions`
gating (`with_deferred_ack` + `validate_deferred_ack`), `MqttClient::subscribe_with_ack`, the
deferred delivery branch (PUBREC withheld until `ack()`), the reconnect DUP re-ack matching the
recorded resolution, and the Receive-Maximum slot wiring. Unit-tested (deferred delivery withholds
PUBREC until ack; `Drop` emits a reason-coded PUBREC; config gating) and clippy-pedantic clean.

**Remaining follow-ups (edge hardening; none block the core feature):**

1. **[CLOSED 2026-07-21] Surface `session_present = 0` on reconnect (§3.5).** Done —
   `on_successful_connect` (`client/inner.rs`) now calls `reset_inbound_state_for_lost_session` when the
   broker reports no session, clearing all inbound `QoS` 2 dedup state via the new
   `SessionState::clear_all_inbound_state` (returns whether anything was present) and emitting a `warn!`
   (deferred ack on) / `debug!` noting outstanding tokens are stale. This runs before the reconnect can
   deliver anything. The clear is unconditional on `session_present = 0` (it also protects plain `QoS` 2
   dedup), not gated on `deferred_ack`; only the warning is deferred-ack-specific. Test:
   `session::state::clear_all_inbound_state_wipes_dedup_and_reports_presence`.

2. **End-to-end reconnect integration test against a live broker.** Exercise the two model regimes
   from `DeferredAckQoS2Reconnect.tla` in real code: (a) `_transport` — a transport reconnect with the
   in-memory session retained delivers exactly once and completes the withheld handshake on the new
   connection; (b) `_crash` — a fresh client on a resumed broker session re-delivers (at-least-once
   processing, no wedge). Current coverage is unit-level only (`handlers.rs` tests); there is no
   broker-loop reconnect test for the deferred path.

3. **[CLOSED 2026-07-21] Re-subscribe ack subscriptions on a clean reconnect.** Resolved on spec
   grounds, not preference: `session_present = 0` on a Clean Start = 0 connect means the broker holds no
   session state, and per §4.1 that state includes the subscriptions ([MQTT-3.2.2-2/-3]). The only
   conformant way to make a subscription active again when the broker has none is to send SUBSCRIBE
   again — exactly what regular subscriptions already do on `session_present = false`. A terminal error
   would be strictly worse and inconsistent. Done — `subscribe_with_ack` now records each ack
   subscription in a new `stored_ack_subscriptions` (topic, options, `CallbackId`), and
   `on_successful_connect` calls `restore_ack_subscriptions_after_lost_session` on `session_present = 0`,
   which re-sends the SUBSCRIBE via `resubscribe_ack_internal` **without** touching the regular
   `CallbackManager` (distinct `CallbackId` space) — the ack callback is already registered in
   `AckCallbackManager` and survives the disconnect. `unsubscribe` now also unregisters the ack callback
   and drops the stored ack subscription, closing a pre-existing gap where `unsubscribe` ignored ack
   subscriptions entirely. Runtime behaviour is verified by the follow-up #2 end-to-end reconnect test.

4. **[CLOSED 2026-07-20] Extend the TLA model to cover packet-id reuse and reject-clears.**
   Done — `DeferredAckQoS2Reconnect.tla` v3 now models reject-clears-state (`[MQTT-4.3.3-9]`), packet-id
   reuse (`ReArm`), scopes the exactly-once invariants to `~wasRejected`, and adds broker self-heal
   actions. Verified across five configs (transport/crash/window pass; persistmistake/buggydup are the
   negative controls) and audited by a 3-way fidelity quorum against the real client + broker code. See
   TLA_DIARY 2026-07-20 (later). Original text kept below for context.

   A post-implementation code review found two bugs (both fixed): the deferred dedup guard
   `inbound_delivered` was not cleared when a QoS2 handshake completed (`handle_pubrel`) or when a
   message was rejected, so a packet id the broker reused for a NEW message was suppressed as a
   duplicate. The fix clears the deferred state at both terminal events. This means the shipped code
   now:
   - gives **at-least-once delivery for REJECTED messages** (a reconnect replay of a rejected PUBLISH
     re-delivers and the app rejects again), whereas the model keeps `resolution="reject"` and
     re-sends the error PUBREC (exactly-once even for rejected); and
   - relies on **packet-id reuse**, which the model does not represent (each id is used once).
   `DeferredAckQoS2Reconnect.tla` therefore proves a *stronger* property than the code delivers for
   rejected messages. Update the model to (a) let a `done` id be re-armed as a fresh instance
   (`toSend`), (b) clear resolution on reject, and (c) scope `InvNoDoubleDelivery` to *acked*
   messages (or a per-instance delivery count that resets on reuse), then re-verify. Exactly-once
   delivery for *acked* messages is preserved and still holds; this is a spec-fidelity gap, not a
   code defect. Regression coverage exists: `deferred_qos2_reused_packet_id_after_completion_is_delivered_again`
   and `deferred_qos2_reused_packet_id_after_reject_is_delivered_again` in `handlers.rs`.

5. **[CLOSED 2026-07-21] Harden broker `next_packet_id` id-reuse (surfaced by the v3 fidelity quorum).**
   Done — `next_packet_id` (`broker/client_handler/lifecycle.rs`) now delegates to a pure
   `next_free_packet_id(start, in_use)` helper that advances the `1..=u16::MAX` counter (wrap `MAX -> 1`,
   never 0) skipping any id present in `outbound_inflight` or `inflight_publishes`, so a `u16` wraparound
   can no longer reissue and clobber a still-outstanding id. Unit-tested in
   `broker::client_handler::lifecycle::tests` (sequential alloc, wrap, skip-in-use, no-clobber-on-wraparound).

6. **[CLOSED 2026-07-21] Clear dedup state in both `handle_pubrel` branches.** Done —
   `handle_pubrel` now clears the inbound dedup state and sends the PUBCOMP unconditionally (the two
   near-identical branches were merged), so a spurious/early PUBREL cannot leave `inbound_delivered` set.
   Test: `deferred_qos2_pubrel_clears_dedup_without_pubrec`.

7. **[CLOSED 2026-07-21] Validate the reason in `AckToken::reject`.** Done — `reject` normalizes any
   non-error reason (Reason Code below `0x80`, e.g. `Success`) to `ReasonCode::UnspecifiedError` before
   emitting, so it can never take the ack path. Test: `deferred_qos2_reject_with_success_reason_still_clears_state`.
