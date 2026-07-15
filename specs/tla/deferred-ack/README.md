# Deferred-Ack — TLA+ model

Formal model of a **deferred-acknowledgement + Receive-Maximum-window** inbound-delivery
mechanism for the `mqtt5` client. It exists to prove a *safe* API boundary before any Rust
is written, in response to GitHub issues #108 / #109 / #110 (see also bug #112).

## Why

A downstream user asked for (a) a raw inbound-PUBLISH callback, (b) a consumer-backpressured
inbound stream, and (c) manual inbound acks. A correctness quorum found that (b) as specified
deadlocks the control plane (one reader task fulfills both PUBLISH delivery and the PUBACK/
SUBACK/PINGRESP that the client's own ops block on), and that (c) is safe only behind a
capability token that owns the window slot and is consumed exactly once. This model pins the
invariants that boundary must satisfy so the implementation can be verified against it, not
guessed.

## Files

- `DeferredAck.tla` / `.cfg` — Stage 1: QoS1 deferred ack + Receive-Maximum window (safety).
- `TLA_DIARY.md` — running log of all modelling work, decisions, and checker results.
  **Read this first**; it carries the full context and the staged plan.

## How to run

Via the TLA+ MCP tooling (preferred — no local install needed):

1. `validate_spec` on `DeferredAck.tla` after every edit. Confirm the expected invariants
   appear in `spec.invariants` (the parser silently drops operators with typos).
2. `check_spec` with an explicit budget, e.g. `max_states=200000, max_depth=60,
   max_seconds=30`. `status: "ok"` = full state space exhausted, no invariant violated.
3. `replay_scenario` to demonstrate specific behaviours (e.g. window backpressure engaging).

Constants live in `DeferredAck.cfg` (`ReceiveMax`, `MaxMsgs`). Keep them small — most bugs
surface at `ReceiveMax = 1..2`, `MaxMsgs = 3`.

## Staged plan

Stage 1 (done): QoS1 safety. Stage 2: control-plane coupling + safe/unsafe variants
(liveness — the #109 deadlock). Stage 3: QoS2 handshake + dedup (catches #112). Stage 4:
token drop semantics + clean/persistent reconnect. See `TLA_DIARY.md` for detail.
