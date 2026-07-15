# Deferred-Ack TLA+ Modelling Diary

Formal modelling of a **deferred-acknowledgement + Receive-Maximum-window** inbound
delivery mechanism for the `mqtt5` client, driven by GitHub issues #108/#109/#110.
The goal: prove a safe API boundary exists (and pin its invariants) *before* writing
any Rust, so we ship a verified capability instead of opening the delivery machinery.

**All work here must be logged in this diary. New entries go on TOP, under the planned-work
list. After every step, every fact learned, every checker run, add an entry.** This
survives context compactions and clears.

---

## Planned work

- [x] **Stage 1 — QoS1 deferred ack + Receive-Maximum window (SAFETY).** DONE (green).
      `DeferredAck.tla`.
- [x] **Stage 2 — control-plane coupling + variants.** DONE. `DeferredAckReader.tla` — SAFE
      holds; UNSAFE (#109-as-specified) shown BOTH as a reachable-state violation and a true
      `<>[]` liveness violation.
- [x] **Stage 3 — QoS2 + dedup.** DONE. `DeferredAckQoS2.tla` — reproduces #112 (Dedup=FALSE)
      and verifies the `has_pubrec` fix (Dedup=TRUE).
- [x] **Stage 4a — clean/persistent reconnect.** DONE. `DeferredAckSession.tla` — persistent =
      no loss but duplicates; clean = silent loss.
- [x] **Stage 4b — AckToken drop semantics.** DONE. `DeferredAckToken.tla` — naive Drop wedges
      the window in 2 steps (silent permanent stall); auto-ack-on-Drop makes it unrepresentable.
      (Bounded-outstanding-error policy deliberately not modelled; not needed for safety.)
- [ ] **Derive the Rust `AckToken` API** from the verified model; document the mapping
      (spec property → API guarantee). ALL MODELLING STAGES ARE DONE; this is what remains.

<details><summary>Original stage descriptions (kept for detail)</summary>

- **Stage 2 detail — control-plane coupling + the two design variants (LIVENESS).** Model the
      single reader task that fulfills both PUBLISH delivery and control-plane acks
      (PUBACK/SUBACK/PINGRESP). Two variants:
      (a) SAFE = backpressure via withholding acks against the window;
      (b) UNSAFE = bounded app channel the reader blocks on (#109 as specified).
      Show (b) violates "delivery/keepalive progress independent of consumer liveness"
      (control-plane deadlock) while (a) does not. This is the formal justification for
      the reply we sent on #110.
- **Stage 3 detail — QoS2 handshake + dedup (SAFETY, catches #112).** Add the four-way
      handshake and a network that can duplicate/lose PUBLISH (DUP redelivery). Prove
      "no dispatch of a packet_id whose PUBCOMP already completed" / no double delivery.
      This invariant FAILS on current `main` (issue #112) and must hold in the model.
- **Stage 4 detail — token drop semantics + clean/persistent reconnect.** Model `AckToken`
      Drop = auto-ack vs bounded-outstanding-error; model session resume
      (clean_start / session_present) to pin exactly when deferred ack yields
      at-least-once *processing* and when a message is lost.

</details>

---

## Context that must survive (from the pre-modelling correctness quorum)

Three independent auditors examined the delivery machinery. Unanimous findings, with
file:line anchors (verify — lines drift):

**Architecture facts (load-bearing):**
1. **One reader task per connection.** `packet_reader_task_with_responses`
   (`crates/mqtt5/src/client/direct/reader.rs:81`) is a single loop that BOTH delivers
   inbound PUBLISH AND fulfills the control-plane oneshots that the client's own
   `publish()`/`subscribe()` block on (PUBACK/PUBREC/PUBCOMP/SUBACK/UNSUBACK), plus
   PINGRESP for keepalive. PUBLISH delivery and control reads share one await point.
2. **Delivery is deliberately decoupled today.** `callback_manager.dispatch`
   (`handlers.rs:150`) pushes onto an *unbounded* channel drained by a *separate* FIFO
   worker (`callback.rs:60-70`) and returns immediately (test at `callback.rs:493`
   asserts a slow callback can't stall the reader). Both requested changes attack this.
3. **Auto-ack precedes delivery.** QoS1 PUBACK (`handlers.rs:95-99`) / QoS2 PUBREC
   (`handlers.rs:139-143`) are written BEFORE `dispatch` at `:150`. Current semantics:
   at-most-once *processing* (crash after ack, before handler, loses the message).
4. **Inbound Receive-Maximum window** lives in `session/flow_control.rs:80-97`
   (`register_inbound_publish` / `acknowledge_inbound`); reject on full →
   `ReceiveMaximumExceeded` → reader loop `Err` → connection teardown (`reader.rs:143-157`).
5. **QoS2 handshake state**: `store_unacked_publish` / `store_pubrec` / `has_pubrec`
   (`state.rs:555-570`), `unacked_pubrels` map. `has_pubrec` consulted only in
   `handle_pubrel` (`handlers.rs:240`), NOT before dispatch → issue #112.

**Why #109-as-specified is unsafe (quorum unanimous):** a bounded stream the consumer
backpressures forces the single reader task to `.await` the consumer; a slow consumer then
stalls PUBACK/SUBACK (own publishes/subscribes time out) and starves PINGRESP → keepalive
kills the connection. Backpressure must be expressed by *withholding acks against the
window*, never by a Rust channel feeding the reader.

**Safe boundary (quorum unanimous):** a capability-style `AckToken` that (a) owns the
packet_id + window slot for its lifetime, (b) is consumed exactly once (double-ack = compile
error via move semantics), (c) has defined Drop semantics, delivered on a task OFF the
reader's critical path. Under this, #109 and #110 collapse into one feature.

**Decisions locked in the #110 reply (already sent to the author):**
- #108 (raw-packet callback): yes, low-risk; `PublishPacket.payload` is `Bytes` (Arc-clone,
  no `to_vec`); may reshape to fit ack story.
- Ack-timing control + real backpressure: yes, but as a verified capability token gated on
  the Receive-Maximum window — NOT `manual_acks(true)` / `client.ack(packet_id)`.
- Consumer-backpressured raw stream (#109 as written): no.
- Naming: "deferred acknowledgement", not "manual acks" (no AMQP nack/requeue in MQTT).
- Deferred ack = at-least-once *processing* only when session survives reconnect
  (`clean_start=false`, not expired, `session_present=1`); consumer must handle duplicates.

**Target invariants (safety):** window bound `|inflight| ≤ ReceiveMax`; at-most-one
PUBACK/PUBREC per delivery; slot released iff owning token consumed (no orphan/double);
no dispatch of a packet_id whose PUBCOMP completed (#112); no app-visible loss across a
*persistent* resume.
**Target liveness:** every accepted PUBLISH eventually delivered (live consumer); outbound-op
+ PINGRESP progress INDEPENDENT of consumer liveness (the property #109-as-specified breaks).

Related: issue #112 (QoS2 duplicate-delivery bug, filed), author reply pasted on #110.

---

## Log (newest first)

### 2026-07-14 — API_DESIGN.md written: model → Rust API mapping. Awaiting review.
All modelling stages done, so derived the Rust API from the verified properties and wrote
`API_DESIGN.md` (property → API obligation table, the API itself, implementation traps, explicit
non-goals, open questions). Highlights:
- `AckToken::ack(self)` by value ⇒ double-ack is a compile error; token not `Clone`; token minted
  only at delivery ⇒ unknown-id acks unrepresentable. No `client.ack(packet_id)`.
- Delivery via `subscribe_with_ack(.., Fn(PublishPacket, AckToken))` on the existing NON-blocking
  dispatch hand-off. This satisfies #108 (raw packet, `Bytes` payload = zero-copy) and #110
  (ack timing) in one call, and refuses #109's consumer-backpressured Stream.
- `impl Drop` = auto-ack (obligation from Stage 4b).
- Gated on a persistent session + an explicit bounded non-zero `receive_maximum`.
- **Nice property that fell out:** graceful drop runs `Drop` ⇒ slot freed ⇒ no wedge; a process
  CRASH never runs `Drop` ⇒ no ack ⇒ redelivery ⇒ at-least-once processing preserved. Deferring
  the ack buys crash-safety and auto-ack-on-Drop prevents the wedge; they don't conflict because
  a crash never runs Drop.
- **Known hole, documented not closed:** `mem::forget(token)` / a leaked Arc cycle leaks the slot
  and silently wedges the subscription. Rust does not guarantee Drop runs. Not preventable in
  safe Rust — the one hazard the type system does not close.
- Prerequisite recorded: fix #112 (has_pubrec guard) BEFORE deferred ack, since deferred ack
  makes DUP redelivery routine.

Open questions parked for the maintainer in §5 of API_DESIGN.md: Drop = ack-success vs
ack-with-reason-code; hard-error vs warn on clean session; per-subscription vs per-connection;
and the QoS2 deferral point (PUBREC vs PUBCOMP) — note deferring PUBREC requires `store_pubrec`
to happen only once the PUBREC is actually written, or `has_pubrec` will lie.

Policy recorded: no updates to #108/#109/#110 until the implementation is complete, tested,
hardened and verified.

### 2026-07-14 — Stage 4b DONE: AckToken Drop semantics. One forgotten token wedges the window.
`DeferredAckToken.tla` + `_leak.cfg` (AutoAckOnDrop=FALSE = naive `Drop` does nothing) /
`_autoack.cfg` (AutoAckOnDrop=TRUE = `Drop` emits the ack). Constants MaxMsgs=2, ReceiveMax=1.

Modelling distinction that matters: **holding a token is legitimate backpressure** (Stage 1
already shows a held token safely throttles the server) — it is NOT modelled as a fault here.
The fault is **abandoning** a token: the consumer no longer holds it (early return, `?`, panic,
forgetfulness) yet the slot is never reclaimed.

Results:
- `_leak.cfg`: **invariant_violation** `InvNoWedge` in TWO steps:
  1. ServerSend(1) → held={1}, inFlight={1}, toSend={2}
  2. AppDropLeak(1) → held={}, abandoned={1}, **inFlight still {1}**
  ⇒ Wedged: |abandoned| = ReceiveMax while toSend ≠ {}. Message 2 can NEVER be delivered —
  `ServerSend`'s guard `Cardinality(inFlight) < ReceiveMax` is false forever and no action can
  free an abandoned slot. **A single forgotten token permanently kills the subscription.**
- `_autoack.cfg`: **ok**, 8 states, full space exhausted; `AppDropAutoAck` fired 4x (drop path
  exercised) and `Done` remains reachable, i.e. delivery still completes despite drops.

**Honesty note on the autoack pass:** `InvNoWedge` holds *trivially* there — `AppDropLeak` is
disabled so `abandoned` is always empty. That is not a deep verification result; it is exactly
the design property we want: auto-ack-on-Drop makes the leak STRUCTURALLY UNREPRESENTABLE
rather than merely unlikely. The substantive evidence is the reachability of `Done`.

**Key insight for the API (revises the quorum's expectation):** the quorum predicted a dropped
token would escalate to `ReceiveMaximumExceeded` → connection teardown. The model shows the
realistic failure against a well-behaved broker is milder to detect but WORSE in practice: a
**silent permanent stall**. Broker and client agree the slot is occupied, so the broker simply
stops sending. No error, no teardown, no log — the subscription is dead while the process stays
up and healthy. Teardown only happens if the broker violates the window. A silent wedge is
harder to diagnose than a crash, which strengthens the case for a defined `Drop`.

API consequences (feed into the Rust design):
1. `AckToken` MUST implement `Drop`. A `Drop`-less token is a wedge waiting to happen.
2. Drop = auto-ack is the safe default: it costs at-most-once for that one message but keeps
   the window honest. Alternative worth weighing: Drop = ack with a non-success reason code,
   which frees the slot AND signals intent instead of silently pretending success.
3. Bounded-outstanding-error (the third policy in the original plan) is NOT modelled and is not
   needed for safety once Drop frees the slot; it would only add earlier detection of a
   consumer that hoards tokens. Deliberately out of scope — noted so we do not imply coverage.

Stage 4 is now complete (4a + 4b). All modelling stages done. Next: derive the Rust `AckToken`
API from the verified model and document the mapping (spec property → API guarantee).

### 2026-07-14 — Stage 4 (reconnect half) DONE: the at-least-once-PROCESSING boundary is proven
`DeferredAckSession.tla` + 3 cfgs. Models deferred ack across a client crash + reconnect: the
app acks ONLY after durably processing; `Disconnect` wipes in-memory tokens (`heldTokens`,
`processedTokens`) but `processed` is durable and the broker's `serverUnacked` (session state)
survives; `Reconnect` keeps `serverUnacked` when `SessionPersistent`, DISCARDS it when clean.
Three separate cfgs so one violation cannot mask the other.

Results — this is the exact tradeoff we asserted to the author on #110, now formally pinned:
- `_persistent_noloss.cfg`: **ok**, 24 states, full space exhausted. Non-vacuous — 15
  Disconnect / 9 Reconnect transitions explored, so the crash paths really are covered.
  ⇒ **With a persistent session, deferred ack loses nothing.**
- `_persistent_nodup.cfg`: **invariant_violation** `InvNoDuplicateProcessing`, 7-step trace:
  ServerSend → AppProcess (processCount=1, durably processed, NOT yet acked) → Disconnect
  (token evaporates; broker keeps serverUnacked={1}) → Reconnect (persistent) → ServerSend
  (DUP redelivery) → AppProcess → **processCount=2**.
  ⇒ **The price of no-loss is duplicates. Exactly-once PROCESSING is not on offer; the
  consumer must be idempotent or dedup.**
- `_clean_noloss.cfg`: **invariant_violation** `InvNoLoss` in only 4 states: ServerSend →
  Disconnect (never processed) → Reconnect (clean) → broker discards serverUnacked ⇒ message
  is in neither toSend nor serverUnacked and was never processed = LOST.
  ⇒ **With a clean session, deferred ack silently loses messages and buys nothing but
  backpressure.** This is the sharp edge to document on the public API.

Net API consequence: deferred ack MUST be gated on / loudly documented for a persistent session
(`clean_start=false`, session expiry not elapsed, `session_present=1`). Consider refusing to
enable deferred-ack mode on a clean session rather than silently degrading to at-most-once.

**Scope note (honest):** the original Stage 4 plan had TWO halves. This entry covers the
clean/persistent reconnect half only. The **AckToken drop semantics** half (Drop = auto-ack vs
leak → window exhaustion → `ReceiveMaximumExceeded` → teardown; and bounded-outstanding-error)
is NOT yet modelled. Stage 1 covers the benign side (window bound holds, withholding acks is
safe backpressure), but the leak-on-drop failure path is unmodelled. Remaining work.

Next: Stage 4b (AckToken drop semantics), then derive the Rust `AckToken` API from the
verified model.

### 2026-07-14 — Stage 3 DONE: QoS2 model reproduces #112 and proves the has_pubrec fix
`DeferredAckQoS2.tla` + `_buggy.cfg` (Dedup=FALSE = current `main`) / `_fixed.cfg` (Dedup=TRUE
= proposed `has_pubrec` guard). Models the inbound QoS2 four-way handshake with DUP
retransmission and ack loss.

Modelling decision that matters: **`s2c` (server→client) is an ORDERED sequence** because MQTT
runs over TCP. An unordered channel would admit a spurious counterexample even WITH the fix —
PUBREL processed before a stale DUP PUBLISH, clearing `has_pubrec`, letting the DUP be
delivered again. TCP ordering rules that out (the server only appends PUBREL after receiving
PUBREC, and any DUP was appended earlier, so it is consumed first). Acks (`c2s`) stay an
unordered set; their order is irrelevant to this property. Retransmission is bounded by
`sendCount[m] < MaxSends` to keep `s2c` finite.

Results:
- `_buggy.cfg` (Dedup=FALSE, MaxMsgs=1, MaxSends=2): **invariant_violation**
  `InvNoDoubleDelivery`, 5-state trace — and the checker found a SHORTER path than hypothesised
  (no ack loss needed at all):
  1. ServerSendPub(1) → s2c ⟨pub1⟩
  2. ServerSendPub(1) again (DUP retransmit before PUBREC arrives) → s2c ⟨pub1,pub1⟩
  3. ClientRecvPubDeliver → deliveryCount=1, pubrecSent=TRUE, PUBREC queued
  4. ClientRecvPubDeliver → **deliveryCount=2** — VIOLATION.
  The second delivery fires with `pubrecSent[1]` already TRUE: exactly the state the
  `has_pubrec` guard would suppress. **This is issue #112, formally reproduced.**
- `_fixed.cfg` (Dedup=TRUE, MaxMsgs=1, MaxSends=2): **ok**, 22 states, full space exhausted.
  Non-vacuous: `ClientRecvPubSuppressed` fired 3x (guard genuinely exercised), `Done` 3x
  (handshake still completes). `InvDoneImpliesDelivered` also holds — suppressing the duplicate
  does not break completion.
- Confidence run, `_fixed.cfg` at MaxMsgs=2, MaxSends=3: **ok**, 2532 states, full space
  exhausted, guard fired 1216x.

Conclusion: the `has_pubrec`-before-dispatch fix proposed in #112 is verified correct for these
constants — it removes the double delivery without breaking the handshake. Note verification is
bounded (small constants), not a proof for all sizes.

Next: Stage 4 — token drop semantics (AckToken Drop = auto-ack vs bounded-outstanding) +
clean/persistent reconnect, to pin exactly when deferred ack yields at-least-once *processing*.

### 2026-07-14 (later) — `<>[]` fix verified; spec restored to the faithful property
The checker's `<>[]P` (stable-eventually) limitation logged in the previous entry is **FIXED**.
Verified with explicit positive AND negative controls on a throwaway spec (`StableRepro`, x
monotonic 0→1 under WF):
- `<>[](x=1)` → **ok** (correct: reaches 1 and stays).
- `<>[](x=2)` → **liveness_violation** with lasso (correct: never reaches 2). Property echoed
  as `<>[]Eq(Var("x"), Lit(Int(2)))`, proving the inner expression is now actually evaluated
  rather than dropped. `validate_spec` no longer emits the "dropping its inner expression"
  warning.

The negative control is the important one: the old bug's signature was a bogus `ok`, so a
positive-only test would not have distinguished a fix from the bug.

Restored `Liveness == <>[](pongsProcessed = PingBudget)` in `DeferredAckReader.tla` (the
property actually intended: "eventually processed AND stays processed"). This removes the
reliance on the earlier monotonicity workaround. Re-ran both liveness cfgs — results unchanged:
- `_safe_live.cfg`: **ok** (61 states).
- `_unsafe_live.cfg`: **liveness_violation**, same lasso (5-state prefix → 1-state ConsumerIdle
  cycle with pongsProcessed=0), property `<>[]Eq(Var("pongsProcessed"), Var("PingBudget"))`.

Also confirms the OTHER lesson still stands and was NOT a checker bug: `ConsumerIdle` is still
required, and TLA's fairness/stuttering semantics are unchanged — SAFE did not falsely violate,
which it would have if arbitrary stuttering at non-deadlock states were permitted. Modelling an
agent that may pause forever still requires an explicit idle self-loop.

Stage 2 remains fully complete (now with the faithful `<>[]` property). Next: Stage 3 (QoS2 +
dedup, reproduce #112).

### 2026-07-14 — Stage 2 liveness now proven temporally (tooling fixed); two modeling lessons
The TLA+ MCP was fixed (tla-mcp 0.6.7; the `<>` bug reported earlier is resolved — plain
`<>` liveness now checks correctly; verified on the minimal repro and here). Went back and
completed Stage 2 as a TRUE temporal-liveness result, not only the reachable-state invariant.

Added liveness cfgs `DeferredAckReader_safe_live.cfg` / `_unsafe_live.cfg` (SPECIFICATION Spec,
the 4 real invariants, `PROPERTY Liveness`), dropping `InvNoControlStarvation` from them so the
temporal result isn't masked by the safety violation in the UNSAFE run.

Two lessons (both cost a wrong "ok" before being caught — do not repeat):
1. **`<>[]P` (stable-eventually) is NOT supported by this checker.** `validate_spec` now warns
   "temporal pattern <>[]P ... dropping its inner expression", i.e. the property was silently
   vacuous and both variants returned a bogus `ok`. Fix: since `pongsProcessed` is monotonic
   and capped at PingBudget, use plain `Liveness == <>(pongsProcessed = PingBudget)` (reaches ==
   reaches-and-stays here). Always eyeball validate_spec warnings before trusting a liveness ok.
2. **TLC won't stutter forever at a non-deadlock state.** The first UNSAFE liveness run wrongly
   returned `ok` because at the stuck state the ONLY enabled action was the (unfair) `AppAck`,
   so the checker was forced to take it and "rescue" the ping. To model a consumer that stalls
   forever, added `ConsumerIdle == appBuf # {} /\ UNCHANGED vars` (a self-loop enabled while the
   consumer holds tokens). Reader/server weak fairness still forces THEIR progress whenever
   enabled, so SAFE is unaffected; only when the reader is blocked (UNSAFE) does idling win.

Final Stage 2 results (all four cfgs, constants ReceiveMax=2, MaxMsgs=2, Cap=1, PingBudget=1):
- `_safe.cfg` (reachability): **ok**, 61 states.
- `_unsafe.cfg` (reachability): **invariant_violation** `InvNoControlStarvation`, 5-step trace.
- `_safe_live.cfg` (temporal): **ok** — `<>(pongs=PingBudget)` holds; ConsumerIdle present
  (23 transitions) yet reader WF drives the ping through regardless of the stalled consumer.
- `_unsafe_live.cfg` (temporal): **liveness_violation** — lasso: 5-state prefix reaching
  socket ⟨pub2,ping⟩ with appBuf={1} (full), then a 1-state `ConsumerIdle` cycle where
  pongsProcessed stays 0 forever. Formal proof that control-plane progress DEPENDS on consumer
  liveness under the #109-as-specified design, and does NOT under the window-backpressure
  design. This is now both a reachable-state exhibit AND a temporal-liveness proof.

Stage 2 fully complete. Next: Stage 3 — QoS2 four-way handshake + duplicating/lossy network;
dedup invariant "no dispatch of a packet_id whose PUBCOMP completed" (reproduce #112).

### 2026-07-13 — Stage 2 written + checked: SAFE holds, UNSAFE reproduces the control-plane deadlock
`DeferredAckReader.tla` + two cfgs (`_safe`, `_unsafe`). Models the single reader task
consuming an ordered `socket` of PUBLISH + PINGRESP packets, delivering pubs to a stalled-
capable app and processing pings (control-plane progress). One boolean CONSTANT `Blocking`
selects the design: FALSE = safe (delivery never blocks the reader; window is the
backpressure), TRUE = unsafe (bounded app channel `Cap` the reader must await = #109 as
specified). Constants: ReceiveMax=2, MaxMsgs=2, Cap=1, PingBudget=1.

**Tooling caveat (record for next session):** the TLA+ MCP would not check the temporal
`Liveness == <>[](pongsProcessed = PingBudget)` — `check_spec` with `check_liveness=true` and
cfg `PROPERTY Liveness` errored `temporal operator <> reached eval ... don't use it as a
state predicate` (phase: invariant), i.e. its liveness path did not engage through this
interface (tried with and without `allow_deadlock`). Worked around by expressing the hazard
as a **reachable-state SAFETY invariant** instead, which is arguably a stronger exhibit (a
concrete trace, not just a cycle):
- `ReaderStuck` = head is a PUBLISH the reader cannot deliver (Blocking & appBuf full).
- `ControlStarved` = `ReaderStuck` AND a PINGRESP is stranded behind it in `socket`.
- `InvNoControlStarvation == ~ControlStarved`.
Also needed helper `Kind(p) == p.type`: the parser rejects field access on a call result
(`Head(socket).type`) — must access `.field` on an identifier (bind via LET or a helper op).
`Liveness` def is kept in the spec for documentation only, not referenced by any cfg.

Results:
- SAFE (`Blocking=FALSE`): `check_spec` **ok**, full space exhausted — 61 states, all 5
  invariants hold incl. `InvNoControlStarvation` (trivially: `ReaderStuck` is false when not
  Blocking). Even a permanently-stalled consumer cannot starve the control plane.
- UNSAFE (`Blocking=TRUE`): `check_spec` **invariant_violation** on `InvNoControlStarvation`,
  28 states, 5-step trace:
  1. ServerSendPub(1) → socket ⟨pub1⟩
  2. ServerSendPub(2) → socket ⟨pub1,pub2⟩ (window=2, both fit)
  3. ServerSendPing → socket ⟨pub1,pub2,ping⟩
  4. ReaderDeliverPub → appBuf={1} (=Cap, full), socket ⟨pub2,ping⟩
  5. STUCK: head=pub2, buffer full, consumer stalled → reader parked; PINGRESP stranded
     behind pub2. Only exit is AppAck (the unfair consumer) → keepalive dies if it stalls.

This is the formal justification for the #110 reply: a consumer-backpressured stream (#109 as
specified) makes control-plane starvation REACHABLE; the window-backpressure design makes it
UNREACHABLE. Stage 2 complete.

Next: Stage 3 — QoS2 four-way handshake + a duplicating/lossy network; add the dedup
invariant "no dispatch of a packet_id whose PUBCOMP completed" and confirm it FAILS without a
`has_pubrec`-style guard (reproducing issue #112) and HOLDS with it.

### 2026-07-13 — Stage 1 spec written, validated, model-checked GREEN
`DeferredAck.tla` + `.cfg` written. Models server → client QoS1 stream with deferred ack and
a Receive-Maximum window; channels are sets of ids (each id produced/acked once, so no bag
semantics needed until Stage 3). Actions: `ServerSend` (guarded by
`Cardinality(serverUnacked) < ReceiveMax`), `ClientReceive` (registers slot + delivers),
`AppAck` (single-shot: requires + removes from `clientWindow`, so double-ack is structurally
impossible — this is how the capability token's exactly-once consumption is represented),
`ServerReceiveAck`, `Terminating` (stutter at completion to avoid a false deadlock).

Results:
- `validate_spec`: ok, all 6 invariants detected.
- `check_spec` (ReceiveMax=2, MaxMsgs=3, budget 200k/60/30s): **status ok**, full space
  exhausted — 98 states, 193 transitions, max depth 13. All invariants hold:
  `InvServerWindowBound`, `InvClientWindowBound` (`|window| ≤ ReceiveMax`),
  `InvNoAckWithoutDelivery` (`ackedIds ⊆ delivered`), `InvClientWindowDelivered`,
  `InvNoAckedSlotHeld` (`clientWindow ∩ ackedIds = {}`, no orphan slot).
- Non-vacuity check via `replay_scenario`: sent m=1, m=2 without acking → `serverUnacked={1,2}`
  = ReceiveMax → third `ServerSend` DISABLED (only `ClientReceive` available), m=3 stuck in
  `toSend`. Confirms Receive-Maximum backpressure genuinely engages (the model isn't passing
  trivially). This is the SAFE backpressure lever: withholding acks throttles the server, no
  channel blocking involved.

Note on `replay_scenario` syntax: cannot reference the existentially-bound `m` from `Next`;
constrain a pinned action by its effect on primed vars instead, e.g.
`action: ServerSend; 1 \in pubChan'`.

Stage 1 complete. Next: Stage 2 — introduce the single reader task and control-plane packets,
model the SAFE (withhold-ack) vs UNSAFE (bounded reader-fed channel, #109-as-specified)
variants, and check the liveness property "outbound-op + PINGRESP progress independent of
consumer liveness". Expect the UNSAFE variant to show a liveness/deadlock violation.

### 2026-07-13 — Workspace + diary bootstrapped
Created `specs/tla/deferred-ack/`. Recorded quorum context and staged plan above so the
work survives compaction. TLA+ MCP tooling confirmed available (`validate_spec`,
`check_spec`, `list_invariants`, `replay_scenario`, demo tools). Next: write Stage 1 spec
(`DeferredAck.tla` + `.cfg`), validate, run a small `check_spec`, log results here.
