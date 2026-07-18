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

### 2026-07-18 — Stage 5 v2: a focused confirmation quorum found the rebuild still hid the real post-crash case (F1) + a latch bug (F2). Refined; all 4 configs now behave correctly.

Ran a 2-reviewer confirmation quorum on the rebuilt `DeferredAckQoS2Reconnect.tla`. Reviewer 1
verified all 6 original quorum findings are genuinely fixed (line-cited). Reviewer 2 (adversarial)
found:
- **F1 (HIGH, real):** `ClientDedupPersists` was all-or-nothing, so it could not represent the state
  a CORRECTLY-persisting impl hits on a process crash — `delivered` persisted to disk, in-memory
  `AckToken` lost. There the replay is suppressed (guard) yet no token is minted → the message can
  never be resolved → the broker's Receive-Maximum slot **wedges permanently**. The model both hid
  this and mis-framed crash re-delivery as a "bug" when it is the at-least-once-processing contract.
  This also means my API_DESIGN §3.7 note ("`delivered` MUST survive reconnect") was WRONG —
  unconditional survival is exactly the wedge.
- **F2 (MED, model bug):** the `pendingResend` latch could fire a second PUBREL on a LIVE connection
  (AppAck→RecvPubrecOk advances the phase before the resend drains), contradicting the model's own
  no-live-retransmit premise.
- F3/F4/F6 (lower): `InvWindowBound` enforced-by-construction; `InvPubrelAfterResolve` never the sole
  failure; small constants. Reviewer 1 + a hand-check confirmed the `ackedEver` phrasing is SOUND
  (not a whitewash).

**v2 fixes.** Split persistence into two axes: `PersistDelivered` (SessionState dedup bits) and
`PersistToken` (app AckToken, new `hasToken` var). Added `InvNoWedge`. Reframed exactly-once DELIVERY
as transport-only; crash gives at-least-once PROCESSING (double delivery expected, not asserted).
F2 fixed by clearing `pendingResend[m]` on every server phase transition. Bumped default constants to
ReceiveMax=2, MaxReconnects=2 (F3/F6 coverage).

Results (MaxMsgs=2, ReceiveMax=2, MaxReconnects=2; CHECK_DEADLOCK FALSE):
- `_transport` (persist both): **ok** — 9 invariants incl InvNoDoubleDelivery + InvNoWedge (2219 st).
- `_crash` (persist neither): **ok** — InvNoWedge + all safety hold; InvNoDoubleDelivery NOT asserted
  (re-delivery is the at-least-once contract) (3144 st).
- `_persistmistake` (delivered persisted, token lost): **InvNoWedge VIOLATED** — the F1 wedge, now a
  formal negative control proving "don't persist `delivered` without re-minting the token."
- `_buggydup` (BuggyDupPubrec): **InvDeferralHeld VIOLATED** via reconnect DUP.
Also spot-checked earlier at ReceiveMax=2/MaxReconnects=2 and MaxMsgs=3/ReceiveMax=2 — green.

**Design rule pinned for the Rust impl:** keep inbound QoS2 dedup state IN MEMORY only (survives a
transport reconnect, lost with the token on a crash → the two safe regimes), OR if persisting it,
re-mint a token on a DUP of a delivered-but-unresolved id. Never persist `delivered` alone. API_DESIGN
§3.7 corrected accordingly.

### 2026-07-17 — Stage 5 REBUILT faithfully (`DeferredAckQoS2Reconnect.tla`): GREEN, all negative controls fire on the RIGHT invariant via the RIGHT (reconnect) DUP mechanism.

Rebuilt per the quorum + the maintainer's "combined reconnect+slot+QoS2" choice. The old in-connection
model (`DeferredAckQoS2Deferred.tla`) was deleted. The faithful model composes Stage 4a's session
machinery (Receive-Maximum slot, reconnect-driven redelivery, persistent/clean) with the QoS2
four-way handshake and the deferred PUBREC:
- LIVE connection = reliable + ordered; NO in-connection DUP (MQTT-4.4.0-1). s2c/c2s cleared on
  Disconnect.
- ALL redelivery via Disconnect→Reconnect replay of still-`awaitPubrec`/`awaitPubcomp` packets
  (`pendingResend`), mirroring `resend_inflight_messages`.
- Receive-Maximum slot: `Outstanding` = awaitPubrec∪awaitPubcomp, bounded by ReceiveMax; a held
  (unresolved) token keeps its id awaitPubrec, throttling new sends — that IS the backpressure.
- Two client bits: `delivered` (dedup, survives reconnect iff `ClientDedupPersists`) and `pubrecSent`
  (has_pubrec, set at ack, cleared at PUBREL, NOT set at reject). Plus a durable monotone `ackedEver`.
- Reject terminal: error PUBREC → server done, no PUBREL; a DUP-after-reject re-sends the error
  PUBREC (not a fabricated success).

Results (MaxMsgs=2, ReceiveMax=1, MaxReconnects=1; 367 states; CHECK_DEADLOCK FALSE):
- `_correct` (persist, not buggy): **ok** — all 8 invariants hold.
- `_dedupbug` (`ClientDedupPersists=FALSE`): **InvNoDoubleDelivery VIOLATED** — deliver→disconnect→
  reconnect wipes `delivered`→server replays PUBLISH→re-delivered (count=2). The §3.6 hazard via the
  real mechanism. This is the meaningful double-delivery negative control the old model lacked.
- `_buggydup` (`BuggyDupPubrec=TRUE`): **InvDeferralHeld VIOLATED** — reconnect replay → DUP path
  emits PUBREC with resolution=none.

Invariants proven: InvNoDoubleDelivery, InvDeferralHeld, InvNoPubrecOnWireBeforeResolve (wire, not
just the bit), InvPubrelAfterResolve (over durable `ackedEver`, so it survives a client crash yet is
still controlled by BuggyDupPubrec), InvWindowBound, InvResolvedImpliesDelivered, InvDoneImpliesNoPubrec
(catches the reject-path leak). One self-correction during the rebuild: InvPubrelAfterResolve first
fired spuriously in `_dedupbug` because a crash wipes `resolution`; fixed by keying it off the durable
`ackedEver` (the app DID ack pre-crash; the server's advance is evidence). All quorum findings closed.
IMPLEMENTATION NOTE for the eventual Rust: the dedup guard `delivered` MUST survive reconnect (persist
inbound QoS2 state, or keep the SessionState across transport reconnect) — else exactly-once DELIVERY
degrades to at-least-once. This is now `DeferredAckQoS2Reconnect`'s headline obligation.

### 2026-07-17 — Stage 5 (deferred-PUBREC QoS2): built, checked GREEN, then a 3-way quorum found it models a DUP mechanism MQTT-5 FORBIDS. Rebuild required.

Design decision 5.4 (defer the PUBREC to the app ack) invalidated Stage 3's PUBREC-at-delivery
assumption, so I built `DeferredAckQoS2Deferred.tla`: two-bit split (`delivered` dedup guard vs
`pubrecSent`/has_pubrec), PUBREC emitted only at app resolve, DUP-before-resolve emits nothing,
error-PUBREC (reject) terminal. Configs: `_correct` (all 4 invariants hold, 6351 states) and
`_buggy` (`BuggyDupPubrec=TRUE` → `InvDeferralHeld` violated, working negative control).

**Building it already caught one gap:** I modeled PUBLISH retransmission but not PUBREL
retransmission; TLC's deadlock exposed it (server stuck `awaitPubcomp` after a lost PUBCOMP with
nothing to re-drive it). Added `ServerResendPubrel` + `CHECK_DEADLOCK FALSE` (bounded-retransmit
terminals are budget artifacts, not wedges).

**Then, per the maintainer's instruction, a 3-reviewer quorum checked MODELLING FIDELITY (which TLC
cannot). It found the model materially wrong. Verified each against the repo before accepting:**

1. **[VERIFIED, decisive] The DUP mechanism is prohibited by MQTT-5.** My model (and Stage 3)
   generate DUPs via *in-connection* PUBLISH/PUBREL retransmission. `[MQTT-4.4.0-1]`: a
   Server/Client MUST NOT resend on a live connection. This repo enforces it —
   `no_spontaneous_retransmission_on_active_connection` (`crates/mqtt5-conformance/src/conformance_tests/section4_qos.rs:622`).
   The broker resends unacked QoS2 PUBLISH with `dup=true` ONLY in `resend_inflight_messages`
   (`crates/mqtt5/src/broker/client_handler/publish.rs:776`, phase `AwaitingPubrec`), called ONLY
   on reconnect with `session_present=true` (`connect.rs:152-154`). **DUPs are reconnect-driven,
   not in-connection.** The real backpressure is the Receive-Maximum slot, not a retransmit timer —
   so the §3.7 "server keeps retransmitting = backpressure" story is false on a live connection.
2. **[VERIFIED] No slot / Receive-Maximum in the model.** Real code takes a slot at
   `register_inbound_publish` (`flow_control.rs:80`) and frees it at PUBREL via `acknowledge_inbound`
   (`handlers.rs:319`). Deferral does NOT move the free point to `ack()` — it WIDENS how long the
   slot is held. A claim that the slot frees at `ack()` would be wrong. The model omits the window
   entirely, so it cannot verify the actual backpressure/outstanding-token bound.
3. **[TRUE by inspection] Reject path is broken two ways.** `AppReject` sets `pubrecSent` which is
   never cleared (no PUBREL after an error PUBREC) → dangling has_pubrec. And `ClientRecvPubDup`
   keys re-send on `pubrecSent` and always sends `ok=TRUE`, so a DUP after a reject fabricates a
   SUCCESS PUBREC and can drive a rejected message to full PUBREL/PUBCOMP. No invariant caught it.
4. **[TRUE] `InvNoDoubleDelivery` is vacuous here** — `delivered` is monotone/never-cleared, so a
   2nd delivery is unreachable in every config. It's a regression assertion, not a proof. The real
   negative control is a reconnect where the client's dedup state does NOT survive (§3.6) → the
   broker's DUP re-delivers. That belongs in the rebuilt model as a `PersistDedup=FALSE` control.
5. **[TRUE, cheap] Proxy + mislabel:** `InvDeferralHeld` asserts the state bit, not the wire — add
   `InvNoPubrecOnWireBeforeResolve` over `c2s`. And the model comment equates `pubrecSent` with
   `unacked_pubrels`; that map is the OUTBOUND sender path (`state.rs:590`). Inbound uses
   `inbound_pubrecs` only. Fix the comment.

**Conclusion / plan.** Stage 4a (`DeferredAckSession.tla`) already models reconnect-driven
redelivery + the Receive-Maximum slot + persistent/clean toggle correctly, but at QoS1 abstraction
(no four-way handshake, no deferred PUBREC). The faithful Stage 5 must COMBINE Stage 4a's session
machinery with the QoS2 handshake + deferred PUBREC: live connection = reliable/ordered, no
in-connection DUP; ALL redelivery via disconnect→reconnect resend of `AwaitingPubrec` PUBLISH;
dedup guard `delivered` must survive reconnect (`PersistDedup` toggle = the §3.6 hazard and the real
double-delivery negative control); slot held delivery→PUBREL; reject terminal (no has_pubrec, DUP
after reject re-sends the SAME reason code, dedup state discarded). `DeferredAckQoS2Deferred.tla` as
written is NOT to be trusted as the implementation spec — it proves dedup against a transport MQTT-5
forbids. Rebuild pending maintainer greenlight on scope.

### 2026-07-17 — QUIC was never broken: a stale 0-byte file was. Option B IMPLEMENTED + e2e GREEN.
I reported "QUIC connect times out, possibly environmental". Maintainer: "QUIC was tested on this
Mac. If it's not working it's because you broke it." **I had not broken it — and neither had the
environment.** Bisect proved it: on clean `main` (8195528) the same test also "passes" in 0.01s
(impossible — it contains a 200ms sleep) ⇒ pre-existing vacuous skip.

**ROOT CAUSE (quorum, 2 analysts, independently reproduced):** a **stale, gitignored, ZERO-BYTE**
file `crates/mqtt5/mqtt_storage/retained/%24SYS%2Fbroker%2Fversion.json`, dated **Feb 21**:
1. empty file → deserialize error (`file_backend.rs:291`)
2. `?`-propagates out of `get_retained_messages` (`file_backend.rs:422`)
3. → `router.initialize()` (`server.rs:1664`) ⇒ **`run()` returns Err immediately**, dropping every
   listener incl. the QUIC endpoint
4. `ready` is signalled AFTER `initialize()`, so it never fires — but the test's
   `let _ = ready_rx.wait_for(|&v| v).await` **swallows the Err and reports ready**
5. client → dead broker → UDP into a closed socket → the exact 30s `QUIC connect failed: Timeout`
**MY OWN MEMORY WARNED ME: "./mqtt_storage CWD footgun" in [[codebase-architecture]]. I had the note
and did not check it.** Moving the stale dir aside: tests went 0.01s → 0.80s and actually connect.

**SECOND, INDEPENDENT MECHANISM (analyst B):** `server.rs:588-602` `setup_quic` **swallows bind
errors** (`warn!` only) ⇒ `with_config` returns Ok, `ready` fires, NO QUIC LISTENER. Reproduced
byte-identically by squatting the port. And the tests squat their own ports: `broker_handle.abort()`
does NOT release sockets — `run()`'s accept-task `JoinHandle`s are dropped, and **dropping a
JoinHandle detaches rather than aborts**, so zombie accept tasks hold the hardcoded ports. Proof:
propagate the bind error and 4 tests instantly fail with `Address already in use` — they had been
reconnecting to the **zombie first broker**, never testing the restarted one.

**⇒ The QUIC suite has NEVER verified QUIC.** `if connect().is_err() { return; }` + swallowed
`ready` error + detached sockets = a totally dead path reporting green for months.

**OPTION B IMPLEMENTED (this is the MQoQ §9.1.2 fix):**
- `server_stream_manager.rs`: retain the recv half at all 3 `open_bi()` sites and
  `spawn_ack_reader(recv, flow_id, packet_tx)` — reads the client's bare MQTT acks (no flow header
  on the return path) and feeds `packet_tx` tagged with the flow id. `ServerStreamInfo` owns the
  reader `JoinHandle` and aborts it on Drop; the PerPublish/ephemeral reader is detached since that
  stream isn't cached.
- **CORRECTION to a relayed claim:** analyst C said PerPublish's `send.finish()` (`ssm.rs:192`)
  means "no ack path by construction". **WRONG** — `finish()` closes only the SEND direction of a
  bidi stream; the recv half stays live, so PerPublish CAN receive acks. I caught this by reading
  the file rather than trusting the relay.
- Plumbing: `ClientHandler.quic_packet_tx` + `with_quic_packet_tx()`, wired from
  `quic_acceptor.rs` (`packet_tx.clone()`), consumed via a new `build_server_stream_manager()`.

**E2E PROVEN (what the maintainer actually asked for — not just "verified the symptom"):**
- New test `test_qos1_subscriber_over_quic_receives_message` (uses `.expect()`, never skips;
  subscribes with EXPLICIT QoS1 because bare `subscribe()` defaults to QoS0 and cannot exercise
  this path at all).
- Before fix: **left: 0, right: 1** in 2.90s (real connect, real failure).
- After fix: **PASSES in 0.80s** — real handshake, PUBLISH on a server data stream, PUBACK back on
  the same flow, callback fires.
- **Negative control:** removed only `.with_quic_packet_tx(...)` ⇒ **left: 0, right: 1** again;
  restored ⇒ green. The test genuinely exercises the fix.
- All QUIC suites now run FOR REAL: broker_quic 20 ok/3.88s, multistream 23 ok/1.35s,
  migration 5 ok/1.23s (previously 30-60s of timeouts). Clippy pedantic clean workspace-wide.

**STILL OPEN (deliberately not folded in — separate concerns):**
1. **A single corrupt retained file bricks broker startup** (`file_backend.rs:417-429`). This is the
   most serious bug found today — a 0-byte file is a **production startup outage**. Should
   warn-and-skip or quarantine, not `?`-propagate. NEEDS ITS OWN ISSUE/BRANCH.
2. `setup_quic` swallowing bind errors (`server.rs:588-602`, and the same at ~648 for cluster).
3. Tests using `broker_handle.abort()` instead of `shutdown()`; hardcoded ports (24567-24571,
   24601-24605, 24607, 14567) should be port 0 + `local_addr()`.
4. The `if connect().is_err() { return; }` green-washing across the QUIC suites → `.expect()`.
5. `broker_quic_integration.rs:62` `let _ = ready_rx.wait_for(...)` swallows the ready error.

### 2026-07-17 — THE SPEC WAS FOUND. Option B CONFIRMED. My "acks on control" reversal was WRONG.
Maintainer challenged my confidence ("you seem to have only partial knowledge of the flow") and
ordered a quorum. It found **the authoritative MQoQ spec**, which neither of us had been reading:
**`publications/comnet/experiments/MQTT-over-QUIC-spec.pdf`** ("Spec MQTT-next", William Yang,
2024-03-05). There is ALSO a project design doc at the repo root: **`QUIC_IMPLEMENTATION_SPEC.md`**.
READ BOTH BEFORE TOUCHING QUIC AGAIN.

**THE DECISIVE RULE — spec §9.1.2 (verified from the PDF text myself, not relayed):**
> "As QoS > 1 messages track delivery states in the Flow State, the MQTT.PUBACK, MQTT.PUBREL,
> and MQTT.PUBCOMP messages for the same MQTT.PUBLISH message **must be exchanged in the same
> data flow**."

Supporting: **§9.19** table marks PUBACK/PUBREC/PUBREL/PUBCOMP **YES** under *Server Data Flow*.
**§9.2**: "A flow can use one QUIC bidi stream. A flow can use one QUIC unidi stream or **[TBD]** a
pair of QUIC unidi streams." The unidi-pair is TBD/unimplemented ⇒ **a QoS>0 server data flow must
be ONE BIDI STREAM.** `QUIC_IMPLEMENTATION_SPEC.md:35`: "broker-to-client uses **bidirectional
(PerTopic, PerPublish QoS 1+)** or unidirectional (PerPublish QoS 0)."

**CONSEQUENCES — I had this backwards:**
1. **`open_bi()` for QoS>0 (`server_stream_manager.rs:169-178`) is a FAITHFUL SPEC IMPLEMENTATION,
   not an accident.** I called it a "smoking gun" of confusion. Wrong. uni for QoS0 (no acks), bi
   for QoS>0 (acks must return on the same flow). Exactly the spec.
2. **The CLIENT IS ALREADY CORRECT.** `quic_stream_reader_task` (`reader.rs:498,504`) acking on the
   arrival stream is spec-conformant. My proposed "client should ack via `ctx.writer` (control)"
   would have VIOLATED §9.1.2 and broken a correct implementation.
3. **The bug is SOLELY the broker's dropped `_recv`** (`server_stream_manager.rs:80/133/174`). The
   `_recv` underscore-discard is the tell — hidden missing logic (cf. the no-underscore rule).
4. **`ServerDeliveryStrategy::PerTopic` is `#[default]`**, so this is the default path.
5. **Flow ≠ stream.** The concept I kept blurring. A *flow* is bidirectional-capable; a *stream* is
   the transport realization of it.
6. **The comnet paper does NOT override the spec.** `main.tex:189,231` say per-publish is
   "unidirectional" — accurate for its QoS0 benchmark (which is what the code does for QoS0); it
   never engages QoS>0 delivery or ack placement. Incomplete description, not competing design.
   (The paper is arguably wrong-as-written for QoS1/2 and could be corrected.)
7. The maintainer's memory ("user data on streams, protocol on control") is right for
   CONNECT/SUBSCRIBE/PINGREQ but §9.1.2 carves out QoS acks, which are bound to their PUBLISH's flow.

**STALE CITATIONS — do not trust the inline `[MQoQ§4.x]` tags.** They use draft numbering that does
NOT resolve against the real spec (its §4 is "Motivation", §5 "New features"; flows are §9). Mapping:
§4.1→9.5, §4.2→9.4, §4.4→9.8, §4.5.1→9.6.1, §4.5.2→9.6.2/9.6.3, §5→§7/§9.11. `CHANGELOG.md:338,352`
cite "MQoQ §9.16"/"§7.4" and DO resolve — the CHANGELOG tracks the real spec, the code comments don't.

**EMPIRICALLY PROVEN (two independent standalone quinn probes):** quinn 0.11.11
`RecvStream::drop` → `stop(0)` (`recv_stream.rs:534`) ⇒ the client's PUBACK write returns
**`Stopped(0)`**. Not a silent hang — an active, DETERMINISTIC failure (the drop happens before the
client even receives the PUBLISH). Then `handlers.rs:74` `?` aborts BEFORE dispatch at `:98` ⇒ the
message never reaches the subscriber callback, and `reader.rs:515` breaks the loop ⇒ **that topic's
cached stream is dead for the rest of the connection, including QoS0**.

**⇒ Stage 7's axis (`BrokerReadsDataStream` FALSE vs TRUE) WAS THE RIGHT ONE. Option B is correct.**

**EXTRA FINDINGS to fold into the implementation:**
- **PerPublish calls `send.finish()` immediately (`ssm.rs:192`)** ⇒ QoS>0 there has NO ack path by
  construction. Retaining `_recv` alone will NOT fix PerPublish.
- **Retaining `_recv` alone is NOT a fix** — it converts the loud `Stopped(0)` into a silent hang
  (exactly `DeferredAckQuicStreams_current.cfg`). The broker must actually READ it.
- **Dead code proving the unwired half:** `get_publish_flow`/`remove_publish_flow` (`state.rs:695,700`)
  and `QuicStreamManager::send_on_flow` (`quic_stream_manager.rs:315`) have **zero callers**;
  `store_publish_flow` (`handlers.rs:122,174`) writes a map nobody reads.
- Broker outbound QoS state (`publish.rs:679 outbound_inflight`) is **per-session keyed by
  packet_id**, NOT flow-scoped — so correlation of a returning ack is by packet_id.
- The `pending_target_flow` branch (`publish.rs:728`) is checked BEFORE the `ControlOnly` branch
  (`:742`), so a flow-bound subscription forces server-stream delivery **even under ControlOnly**.

**PROCESS LESSON:** I flip-flopped three times (reorder hypothesis → Option B → acks-on-control →
Option B) because I reasoned from code + a benchmark paper while an authoritative spec sat in the
repo unread. The maintainer caught it from a grammar tell of low confidence. **Find the spec first.**

### 2026-07-15 — Stage 7: Option B FIXES the wedge but RE-ARMS the reordering hazard
`DeferredAckQuicStreams.tla` + 3 cfgs. Models inbound QoS2 over QUIC where ONE logical flow spans
TWO independently-ordered streams: PUBLISH + PUBREC on the server-initiated DATA stream (client
writes its ack to the stream the PUBLISH arrived on, `reader.rs:498`), PUBREL + PUBCOMP on the
CONTROL stream (`publish.rs:515`). CONSTANT `BrokerReadsDataStream` = current code (FALSE) vs
Option B (TRUE).

**Result 1 — `_current.cfg` (FALSE): `InvHandshakeCanComplete` VIOLATED.** Client delivers the
message and sends PUBREC; PUBREC is discarded (no reader on the server-opened stream's recv half);
broker sits in `awaitPubrec` FOREVER. The QoS2 wedge, formally reproduced. Confirms the quorum's
code trace.

**Result 2 — `_optionb.cfg` (TRUE, MaxSends=2): `InvNoDuplicateDelivery` VIOLATED.** THIS IS THE
IMPORTANT ONE. Option B fixes the wedge and thereby **RE-ARMS the exact cross-stream reordering
hazard I hypothesised and the quorum refuted**. 6-step trace:
  1-2. Broker sends PUBLISH then a DUP → two in flight on the data stream
  3. Client delivers (deliveryCount=1), sets pubrecSent, sends PUBREC — **DUP still in flight**
  4. Broker reads PUBREC (only possible under Option B!) → sends PUBREL on the CONTROL stream
  5. Client processes PUBREL → **pubrecSent=FALSE** — dedup state cleared
  6. The stranded DUP arrives → looks like a first receipt → **deliveryCount=2**

**Result 3 — `_optionb_nodup.cfg` (TRUE, MaxSends=1): ok.** Cause isolated: the hazard needs an
**in-connection DUP retransmit on the data stream**. The quorum established none exists today (the
only DUP is `resend_inflight_messages` on session resume, and it goes over the CONTROL stream at
`publish.rs:790`).

**CONCLUSION — the quorum's refutation and my hypothesis were BOTH right, about different worlds.**
The refutation was correct *for current code* (no in-connection retransmit ⇒ unreachable). My
hypothesis was correct *for the design*: **delete-on-PUBREL is unsound under a two-stream topology
whenever a DUP can traverse the data stream.** Today it is saved only by the accident that no
retransmit timer exists AND that `publish.rs:790` bypasses `write_publish_bytes` to use the control
stream — a line the earlier entry already flagged as looking ACCIDENTAL. Two independent accidents
are the only thing standing between us and duplicate delivery.

**DESIGN CONSEQUENCE for Option B (do NOT skip):** shipping Option B alone leaves a landmine — the
first person to add a retransmit timer, or to "tidy" `publish.rs:790` onto the strategy-aware path,
silently re-introduces duplicate delivery with no test to catch it. Option B must therefore be
paired with ONE of:
  (a) do not clear the inbound dedup marker on PUBREL — retain completed packet ids until the id is
      provably released (costs memory; needs a bound);
  (b) guarantee a DUP PUBLISH always travels the SAME stream as the PUBREL that releases it (i.e.
      route retransmissions over the control stream **by contract, not by accident**, and assert it);
  (c) carry ack routing in the model of record and add a regression test that a DUP arriving after
      PUBREL is not re-delivered.
Recommend (b) + (c): cheapest, matches today's de-facto behaviour, and turns an accident into an
invariant. Re-run `_optionb.cfg` at MaxSends=2 after implementing — it must go green.

Also still open from API_DESIGN §3.1: it assumes ONE writer for the AckToken's ack. Under QUIC
there are **N writers** (one per stream) and the token must ack on the stream its PUBLISH arrived
on. Option B is exactly the machinery that makes that possible — deferred ack and the QUIC ack-
routing fix are coupled and must land together.

### 2026-07-15 — `main` was silently RED for 6 days; CI cannot see integration tests
Running the FULL suite (not `--lib`) surfaced `test_maximum_packet_size`
(`integration_mqtt5_features.rs`) failing. Proved it **pre-existing** by stashing all my work and
re-running on clean HEAD — fails identically. Then found the real story:

**It is NOT a regression — it is an OBSOLETE TEST asserting behaviour that was deliberately
removed.** The test set the *client's* `maximum_packet_size = 1024` and asserted its own 2KB
publish failed. But **mqtt5-protocol 0.14.2 / mqtt5 0.36.1 (2026-07-09)** deliberately changed
exactly this: "`effective_maximum_packet_size` no longer clamps outbound packets by the client's
own inbound limit … the client's Maximum Packet Size is what it will *receive*, not what it may
*send*". Code confirms: `effective_maximum_packet_size` returns the SERVER's limit when present,
falling back to the client's only if the server advertises none — and the broker DOES advertise
one (`broker/client_handler/connect.rs:394`), defaulting to 268_435_456 (`config/mod.rs:203`).
So server 256MB wins, client's 1024 correctly ignored, 2KB publish succeeds, `assert!(is_err())`
fails. The test should have been updated in that PR and wasn't.

**Rewrote it** to test the real semantics: configure the BROKER via
`TestBroker::start_with_config(BrokerConfig::default().with_max_packet_size(1024))`, assert an
oversized publish fails locally with `MqttError::PacketTooLarge`. **Validated with a negative
control**: raising the broker limit to 64KB makes it FAIL (publish succeeds); back to 1024, it
passes. So it genuinely exercises the limit rather than passing vacuously.

**RESOLVED — integration tests added to the gate.** New `test-integration` task
(`cargo test -p mqtt5 --tests`, deps `build-cli` since `cli_functionality.rs` lives in
`crates/mqtt5/tests/`), wired into `ci-verify` and `.github/workflows/rust.yml`. It SUBSUMES the
old `test-quic` step (`broker_quic_integration.rs` is in the same dir and `transport-quic` is a
default feature), so that step was replaced rather than added to.
Measured cost, warm cache: old gate step `test-quic` = **73s**; new `test-integration` = **188s**
⇒ **+115s (~2 min) per PR commit**. NOTE: I first told the maintainer "+30s" — WRONG. 90s was
*test execution*; the real driver is **linking 37 test binaries**. Corrected before the decision
was final. The 60s outlier is `broker_quic_integration` (19 tests) — already paid by today's gate.
Maintainer's call: keep full integration on every PR commit.

**THE SYSTEMIC FINDING — the CI gate could not see integration tests.**
`.github/workflows/rust.yml:89` runs `cargo make test-fast` = `cargo test --lib --bins`
(`Makefile.toml:98`), and `ci-verify` (`:136`) depends on the same `test-fast`. **`tests/` is
never run by the PR gate.** Only `dependencies.yml` runs `cargo test --all-features`, and it is
not the gate. Consequences seen in one session:
- an obsolete test rotted red on `main` for 6 days unnoticed;
- my own red property test (previous entry) sailed past `cargo test --lib`.
This is why "it passes" meant nothing. **Always run the integration targets before claiming
green.** Worth its own issue: make the PR gate run integration tests.

### 2026-07-15 — `mqtt5::tasks` REMOVED (0.38.0) + verification quorum caught 6 problems in my work
Quorum-confirmed `mqtt5::tasks` should go (2 analysts + the MQDB check), then removed it. A
SECOND verification quorum on my own removal found **6 problems, one of them a red test I had
already declared green**. Recording all of it.

**Why removal (settled):** internally dead (zero refs workspace-wide); **never wired up in the
crate's entire history** (`git log --all -S"tasks::"` empty across all branches; born in initial
commit `e340249`); **MQDB — the one real downstream consumer — does not use it** (checked
`/Volumes/SanDisk 4TB/repos/mqdb`: 0 hits; it imports broker/types/client/time/telemetry only,
and is pinned at mqtt5 0.35.1 anyway). My premise that it was *uncallable* was **REFUTED** — both
analysts compiled an external crate against it; all param types are public and constructible. The
real argument is the inverse: **`mod direct;` is private (`client/mod.rs:27`), so `tasks` was the
only public low-level packet path — and it was the broken one.** ~10 divergences incl. duplicate
QoS2 delivery, no flow control, no codec decode, no stream_id, PINGRESP no-op (keepalive could
never detect a dead peer), vacuous tests.

**PROBLEMS THE VERIFICATION QUORUM FOUND IN MY OWN WORK (all fixed):**
1. **RED TEST I shipped.** `tests/session_state_property_tests.rs::prop_qos2_state_transitions`
   FAILED. It modelled the OUTBOUND flow but called the INBOUND accessor `store_pubrec`; once the
   maps split, `complete_pubrel` no longer cleared it. **My `cargo test --lib` was a FALSE GREEN.**
2. **`cargo make ci-verify` IS ALSO BLIND TO THIS.** `Makefile.toml:136` ci-verify → `test-fast`
   = `cargo test --lib --bins` (`:98`) — **excludes `tests/`**. The repo's own gate would not have
   caught it. `.github/workflows/rust.yml` runs the same. **Never trust `--lib` alone again; run
   the integration targets.**
3. **FALSE CHANGELOG CLAIM.** I wrote that the dedup check "previously took separate locks",
   implying a fixed race. **There was no check in 0.37.2 at all** — it dispatched unconditionally.
   I invented a prior bug. Reworded to state the new code's guarantee instead.
4. **Dependent versions not bumped.** cli 0.28.3 / wasm 1.4.3 must bump (0.28.4 / 1.4.4) — 6 of 7
   prior minor releases did so, and `.github/workflows/release.yml:35,43` uses
   `cargo publish || echo "Version already published, skipping"`, so **the release would have
   silently shipped nothing for them and left them pinned to mqtt5 0.37 forever.**
5. **`ARCHITECTURE.md:46` still advertised the module** (I only fixed the `:110` diagram). It also
   miscredited `tasks` with "reconnection" it never had.
6. **The split was INCOMPLETE — same bug class I claimed to fix.** `ack_qos2_inbound` stored the
   INBOUND publish into `unacked_publishes`, the OUTBOUND retransmit map: collides on packet id
   AND **leaked a full payload per inbound QoS2 message** (nothing on the inbound path removed
   it). Deleted the store entirely — Method A delivers on first receipt, so the payload is never
   needed again.

Also: `store_pubrec` became orphaned (zero production callers; superseded by
`mark_pubrec_pending`) and its name was direction-ambiguous — **that ambiguity is exactly what
caused problem 1**. Removed it (BREAKING, documented). Rewrote the property test into three:
outbound transitions, inbound transitions, and `prop_qos2_directions_do_not_collide`.

Released as **0.38.0** (breaking: `pub mod tasks` + `SessionState::store_pubrec` removed), with
CHANGELOG Removed/Fixed/Changed entries incl. the `SessionStats::unacked_pubrel_count` semantic
change.

**LESSON (the important one):** I ran a quorum on the *investigation* and it saved me; I did NOT
run one on my *own implementation* until told to — and it found a red test, a false changelog
claim, and an incomplete fix. Verify your own work as adversarially as you verify code you are
reviewing. Also: give parallel agents separate scratch dirs (two agents collided in a shared
scratchpad during the tasks investigation).

Still open (noted, not fixed): `flow_control.rs:86` checks `len() >= max` before insert with no
`contains_key` guard, so at exactly receive_maximum a legitimate DUP of an already-tracked id
returns `ReceiveMaximumExceeded` → connection teardown, making the duplicate path unreachable at
capacity. Pre-existing; latent today only because `inbound_receive_maximum` is hardcoded 65535
(`set_inbound_receive_maximum` is never called). Goes live the moment deferred-ack wires up a
bounded window (API_DESIGN §3.3). Also `handle_pubrel`'s if/else branches are identical apart
from `remove_pubrec`.

### 2026-07-15 — Stage 6 DONE + shared-map bug FIXED (`inbound_pubrecs`)
`DeferredAckBidir.tla` + `_shared.cfg` / `_separate.cfg`. Models BOTH QoS2 directions with
independent packet-id spaces (MaxId=1 suffices — the collision is on the SAME id). CONSTANT
`SharedMap` selects current-code vs fix. Key operator:
`EffectiveInbound == IF SharedMap THEN inboundKeys \cup pubrelKeys ELSE inboundKeys` — i.e. with
one map the inbound dedup check also sees OUTBOUND PUBREL state.

Results:
- `_shared.cfg` (current code): **invariant_violation** `InvNoSuppressedFirstDelivery` in 3
  steps — OutboundPublish(1) → OutboundRecvPubrec(1) [pubrelKeys={1}] → InboundPublish(1) →
  `deliveryCount` stays **0**, `wronglySuppressed={1}`. A genuine first receipt, judged a
  duplicate, never delivered. **Silent message loss reproduced formally.**
- `_separate.cfg` (fix): **ok**, 20 states, full space exhausted.

**The model's FIRST CUT WAS WRONG and the checker caught me.** My initial `InvNoDuplicateDelivery`
conflated "same message redelivered" with "packet id REUSED for a new message after the handshake
completed". It reported a false violation on the FIXED variant (deliver → PUBREL → deliver again).
Legitimate id reuse is exactly what the Rust test
`qos2_packet_id_is_deliverable_again_after_the_handshake_completes` asserts. Fixed by adding
`inboundCompleted` and barring re-publish after completion. LESSON: had I trusted that "failure"
I'd have concluded the fix was broken and chased a non-bug. Always ask whether a counterexample
is a real defect or a modelling artifact — Stage 3 avoided this only because its server state
machine implicitly forbade the re-send.

**Rust fix applied:** new `inbound_pubrecs: Arc<RwLock<HashMap<u16, Instant>>>` on `SessionState`,
separate from `unacked_pubrels`. Repointed the four INBOUND accessors (`store_pubrec`,
`mark_pubrec_pending`, `has_pubrec`, `remove_pubrec`) at it. Left the OUTBOUND ones
(`store_unacked_pubrel` / `remove_unacked_pubrel` / `get_unacked_pubrels` / `store_pubrel` /
`complete_pubrel`) on `unacked_pubrels`. Also added `inbound_pubrecs` to `clear()` — without it
inbound dedup state would survive a session clear and suppress live messages on a fresh session
(a bug the fix would otherwise have introduced).
Verified caller classification: `tasks.rs:153` store_pubrec = inbound ✓, `tasks.rs:192`
store_pubrel = outbound ✓.

New Rust regression test `outbound_pubrel_does_not_mask_an_inbound_packet_id`, validated by
reverting the fix: fails `left: 0, right: 1` (never delivered = silent loss). 437 lib tests pass,
clippy pedantic clean.

**NEW FINDING — `mqtt5::tasks` is a second inbound QoS2 path with the SAME #112 bug.**
`crates/mqtt5/src/tasks.rs` is `pub mod` (lib.rs:194) with `pub async fn packet_reader_task`, but
**nothing internal references it** (the mqtt5-wasm hits are its own separate modules). Its
`handle_publish` (`tasks.rs:113`) sends PUBREC then calls `route_message` (`:164`)
UNCONDITIONALLY — no dedup guard. So it is public-but-internally-dead API carrying the bug.
Decide: fix the guard there too, or remove the module (breaking change — it is public API).
NOT yet addressed.

### 2026-07-15 — QUORUM ON QUIC: my reordering hypothesis REFUTED; two real bugs found instead
Ran a 3-analyst quorum (2 independent + 1 adversarial) over the QUIC QoS2 path after the
maintainer noted I overlook things. It refuted my central hypothesis AND found a bug in my own
fix. Most important entry in this diary.

**1. My cross-stream reordering hypothesis: REFUTED (3/3).** I claimed PUBLISH-on-data-stream +
PUBREL-on-control-stream lets a DUP PUBLISH be processed after PUBREL, defeating
delete-on-PUBREL dedup. Wrong, on three independent grounds:
- The ONLY DUP PUBLISH is `resend_inflight_messages` (`publish.rs:782`), called solely from
  `connect.rs:154` on **session resume**, and it writes via `self.transport.write`
  (`publish.rs:790`) = **control stream** — same stream as PUBREL ⇒ ordered.
- There is **no in-connection retransmit timer** anywhere (QUIC streams are already reliable).
- Causality: PUBREL only follows PUBREC, which only follows the PUBLISH.
⇒ delete-on-PUBREL is sound here. **FRAGILE CAVEAT:** this rests on `publish.rs:790` bypassing
the strategy-aware `write_publish_bytes`, which looks ACCIDENTAL. "Tidying" that line would make
the hazard real. Watch it.

**2. Stage 3's justification was WRONG though its conclusion survives.** `DeferredAckQoS2.tla`'s
header says `s2c` is ordered "because MQTT runs over TCP". False — this library supports QUIC
with multiple streams. Ordering holds for a DIFFERENT reason (DUP travels the control stream).
Right answer, wrong reason. Do not trust that header.

**3. A REAL bug in my own #112 fix (independently verified): shared `unacked_pubrels`.**
`store_pubrel` (`state.rs:595`, called from `handle_pubrec_outgoing` `handlers.rs:274` — the
OUTBOUND direction) writes to the SAME `unacked_pubrels` map the INBOUND dedup
(`mark_pubrec_pending`/`has_pubrec`/`remove_pubrec`) uses. MQTT packet ids are **independent per
direction** and both allocators start at 1 (`packet_id.rs:21`, `client_handler/mod.rs:165`).
So: we publish QoS2 id 5 → PUBREC → `store_pubrel(5)` occupies the map → an inbound QoS2 PUBLISH
id 5 → `mark_pubrec_pending(5)` sees `insert` return `Some` → judged Duplicate → **dispatch
suppressed → message silently LOST**. My fix would have turned a duplicate-delivery bug into a
message-LOSS bug. Fix: the inbound dedup needs its OWN map.

**MODEL BLIND SPOT:** `DeferredAckQoS2.tla` models only the INBOUND direction. No outbound flow
⇒ no second packet-id space ⇒ the collision is structurally unrepresentable. The model COULD NOT
have found this. ⇒ Stage 6.

**4. A bigger, separate bug: QoS>0 broker→client delivery over QUIC is BROKEN by default.**
- `ServerStreamManager` opens bi streams (`server_stream_manager.rs:80/133/174`) and drops the
  recv half (`let (mut send, _recv) = ...open_bi()`). **No reader exists over them anywhere.**
- quinn 0.11.11 `RecvStream::drop` → `stop(0)` (`recv_stream.rs:534`) = **STOP_SENDING(0)**, so
  the client's PUBACK/PUBREC write actively FAILS (not silently discarded).
- The ack is `?`-propagated BEFORE dispatch (`handlers.rs:74`/`:81` vs `:99`) ⇒ the message
  **never reaches the application**; `reader.rs:515` then breaks the reader task. Under
  `PerTopic` the topic stream is cached ⇒ **every later message on that topic, incl. QoS0, is
  lost for the rest of the connection.**
- `ServerDeliveryStrategy::PerTopic` is the **unconditional default** (`config/transport.rs:9`).
  No QoS gate, no opt-in, no fallback except `ControlOnly`.
- Intent is explicit: `write_on_ephemeral_stream` picks `open_uni()` for QoS0 but `open_bi()`
  for QoS>0 — it MEANS to receive acks — then discards the read half.

**TLA+ COULD NOT HAVE CAUGHT #4.** Models assume channels deliver; "opened a bi stream and threw
away the read half" is a wiring defect below the abstraction line. Code reading found it.
Remember this when judging what modelling buys.

**5. Why nothing caught this: the test suite cannot see it.**
- `MqttClient::subscribe()` defaults to **QoS0** (`SubscribeOptions::default()`), so every QUIC
  test silently downgrades broker→client delivery to QoS0.
- The only QoS>0 QUIC subscribe tests (`quic_integration.rs:123,177`) are `#[ignore]` AND target
  an external EMQX broker — they never run, never touch our broker.
- Many QUIC tests early-`return` on connect failure ⇒ **vacuous passes**.
- `test_quic_mixed_qos_with_streams` asserts `received >= 2` of 3 ⇒ passes if a message is
  dropped AND if one is delivered twice. Structurally incapable of catching this.

**DECISIONS (maintainer):** fold the QUIC fix into this branch even though it is a different bug
— not shipping an incomplete fix. QUIC fix shape = **Option B**: broker spawns a reader over the
recv half of each server-opened stream (preserves the multi-stream / MQoQ head-of-line benefit).
Option A (route QoS>0 over the control stream) rejected: it defeats per-topic streams for
exactly the traffic that benefits most.

Branch topology: `fix-qos2-duplicate-delivery` rebased onto `deferred-ack-spec` so the model and
the fix it justifies travel together.

**PLAN:**
- [x] Stage 6 — DONE, see entry below.
- [x] Fix: inbound-only dedup map (`inbound_pubrecs`) — DONE, see entry below.
- [ ] Stage 7 — model the Option B topology: PUBLISH on data stream + ack on data stream +
      PUBREL on control stream; re-check dedup invariants under genuine multi-stream.
- [ ] Implement Option B in the broker.
- [ ] Tests exercising QoS>0 over QUIC (close the coverage hole).

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
