# Offline queue and session resume: TLA+ model

Model of the `mqtt5` client's outbound delivery across disconnects: the offline publish queue,
Session Present = 1 replay, Session Present = 0 recovery, and how each accepted QoS 1/2 publish
reports its outcome. It combines three independent models (a quorum) into one spec. The main
design is the one the user chose. The current code is kept as the variant `Design = "D0"` so
that the spec reproduces its bugs.

## Files

- `OfflineQueue.tla`: the spec, containing the chosen design (`Design = "Fix"`) and the current code (`Design = "D0"`).
- `OfflineQueue.cfg`, `OfflineQueue_rm2.cfg`, `OfflineQueue_qos.cfg`, `OfflineQueue_size.cfg`, `OfflineQueue_alldims.cfg`: main safety runs of the chosen design, split by dimension.
- `OfflineQueue_live.cfg`: liveness of the chosen design.
- `OfflineQueue_NEG_*.cfg`: negative controls, which must fail.
- `OfflineQueue_D0_*.cfg`: the current code, one cfg per property it breaks.
- `OfflineQueue_V_*.cfg`: rejected alternatives to the chosen design. Each one shows why the chosen mechanism is needed.

## The chosen design

1. **Per-publish outcome.** Every accepted QoS 1/2 publish gets its own completion handle. That
   handle settles exactly once, to one of these outcomes:
   - `Delivered{qos_used}`
   - `Rejected(reason)`: the message was definitely not delivered.
   - `Indeterminate(reason)`: the message may have been delivered.

   Outcomes are keyed by publish, never by packet id. The same rule covers live (online)
   publishes and queued ones. If the connection drops while a live publish is in flight, its
   handle stays pending and settles later.
2. **Enqueue-time checks.** A publish that breaks the last-known Retain Available or Maximum
   Packet Size is rejected synchronously. Before the first CONNACK no limits are known, so this
   check is permissive. A publish is also rejected synchronously when no packet id is free.
3. **Flush after replay, against the new CONNACK.** A queued message that no longer conforms
   (RETAIN when Retain Available = 0, or larger than the new Maximum Packet Size) is handled like this:
   - It is removed from the queue.
   - Its packet id is freed.
   - Its outcome is Rejected. If it was requeued after Session Present = 0 (so it may have been
     delivered before), its outcome is Indeterminate instead.
   - Flushing continues. There is no hold and no reordering.

   If the new Maximum QoS is lower than the requested QoS, the message is downgraded and sent. Its
   outcome reports the QoS actually used. A downgrade to QoS 0 settles as `Delivered{0}` as soon
   as it is written. `qos_used = 0` means unconfirmed.
4. **Session Present = 1 replay.** Each stored message is re-checked against the new CONNACK:
   - A stored message in the PUBLISH stage that no longer conforms is not sent. Its outcome is
     `Indeterminate(ReplayNotConforming)`.
   - If that message was QoS 2, its packet id is **quarantined**: the id cannot be allocated
     again until a Session Present = 0 connection.
   - A stored QoS 2 message in the PUBREL stage is replayed as a PUBREL. PUBREL is not subject to
     Retain Available, Maximum QoS or Maximum Packet Size.
5. **Session Present = 0.**
   - Unacked messages that went out on the wire as QoS 1 return to the head of the queue in their
     original order. They are then sent as new publishes: DUP=0, and new ids are allowed.
   - Unacked QoS 2 messages in the PUBLISH stage get outcome Indeterminate.
   - QoS 2 messages in the PUBREL stage get outcome `Delivered{2}`. Once the receiver has sent
     PUBREC Success, it owns the message.
   - Quarantine is cleared.
6. **Race-freedom.**
   - Replay and flush work is bound to its connection. When a new connection starts, the task
     from the old connection is killed.
   - The step that takes a queued message is atomic: it removes the message from the queue by
     identity, puts it in the session store and writes it. That step runs only on the current
     connection.
   - Packet ids of messages being conformed, stored or quarantined are excluded from allocation.
7. **Receive Maximum and ordering.**
   - Receive Maximum is respected throughout, including during replay. A QoS 2 message holds its
     slot until PUBCOMP.
   - First receipts at the server follow publish order: replayed messages first, then queued
     ones, then new live publishes.

### Clarifications the design needed

- **Replay downgrade (item 4).** A stored message whose QoS is above the new Maximum QoS is
  treated as non-conforming. Its outcome is Indeterminate, and a QoS 2 id is quarantined. It is
  not downgraded. The reasons:
  - [MQTT-4.4.0-1] requires unacked PUBLISH packets to be resent with their original packet
    identifiers, with DUP=1. The server may still hold QoS 2 state for that id: it received the
    PUBLISH, but its PUBREC was lost. A QoS 1 PUBLISH under the same id would reuse an id that is
    still in use by an unfinished QoS 2 exchange (a packet id stays in use until PUBCOMP). The
    server would deliver the message again through its QoS 1 path and leave the QoS 2 state
    orphaned.
  - A QoS 0 resend is not a retransmission at all. QoS 0 forbids DUP=1 [MQTT-3.3.1-2] and has no
    packet id.
  - Sending above the new Maximum QoS is forbidden [MQTT-3.2.2-11].
  - So no spec-legal downgrade of an already-sent packet exists. The rejected alternative is
    `ReplayDowngrade = TRUE` (`OfflineQueue_V_replaydowngrade.cfg`). The checker finds the
    orphaned server QoS 2 state right away (`InvNoStaleServerPid`).
- **Downgrade and ExactlyOnce.** `qos_used` is the *lowest* QoS the message was ever sent at
  (`effQ`). A message requeued after Session Present = 0 is re-flushed at
  `Min(effQ, new Maximum QoS)`. It is never raised back to QoS 2, because a message that may
  already have been delivered through the QoS 1 path cannot be made exactly-once afterwards. The
  exactly-once guarantee applies only to messages sent at QoS 2 on every transmission. A message
  received more than once must have `effQ < 2`, and any Delivered or Indeterminate outcome must
  report `qos_used < 2`.
- **"Unacked QoS 1" at Session Present = 0** means QoS 1 *on the wire*. A QoS 2 request that was
  downgraded to QoS 1 is therefore requeued.
- **PUBREL-stage replay** takes a Receive Maximum slot, because the message stays
  unacknowledged until PUBCOMP. Replayed PUBRELs therefore never push the window over a smaller
  new Receive Maximum.

## Model

- **Messages.** Messages `1..N` are published in order. Each has a requested QoS (`reqQ`), a
  RETAIN flag (`reqR`) and an "oversize for a restrictive Maximum Packet Size" flag (`big`),
  all chosen in `Init`.
- **Connections.** Each connection gets a CONNACK with Maximum QoS, Retain Available,
  Maximum Packet Size, Receive Maximum and Session Present. Up to `MaxConns` connections happen.
  Only a connection before the last one can be lost, so the last connection stays up.
- **Network.** The network is FIFO. A lost connection drops everything in flight.
- **Server.** The server delivers a PUBLISH when it receives it. It de-duplicates QoS 2 by
  packet id (`srvQ2`) until it receives PUBREL.
- **QoS 2 handshake.** It is modelled as three steps: PUBREC, then the client's PUBREL, then
  PUBCOMP.
- **Reporting.** `outcome[m]` and `outQ[m]` are the caller's view of message `m`. With
  `Report = "pidEvent"` (a rejected alternative), results are instead emitted as events keyed by
  packet id. The caller maps each event to whichever publish it last received that id for.

## Properties in plain words

All of these are invariants checked in every run of the chosen design.

| Property | Meaning |
|---|---|
| `TypeOK` | Variables stay within their bounded types. |
| `InvRetainAvailable` | No PUBLISH on the wire (including replay) sets RETAIN while the current CONNACK says Retain Available = 0. |
| `InvMaximumQoS` | No PUBLISH on the wire exceeds the current Maximum QoS. |
| `InvMaximumPacketSize` | No PUBLISH on the wire exceeds the current Maximum Packet Size. |
| `InvReceiveMaximum` | The client never has more unacknowledged QoS 1/2 messages than the current Receive Maximum. |
| `InvNoSilentLoss` | An accepted message is in one of these situations: still held by the client, received by the server, or in flight. Otherwise its outcome says Rejected, Indeterminate or `Delivered{0}` (unconfirmed). "Delivered" at QoS > 0 for a message the server never received is a violation. This also catches a message swallowed by server de-duplication under a reused id. |
| `InvRejectedNeverDelivered` | A Rejected message was never received and is not in flight. |
| `InvOrder` | First receipts at the server are in publish order. |
| `InvPidUnique` | No two messages that the client still holds (queued, being conformed or popped, or stored) share a packet id. |
| `InvPidNotQuarantined` | No held message uses a quarantined id. |
| `InvNoStaleServerPid` | A packet on the wire never uses an id for which the server holds QoS 2 state belonging to a different message (or uses it at a QoS other than 2). No held message owns such an id. |
| `InvNoMisattribution` | An outcome is never delivered to the wrong publish. |
| `InvExactlyOnce` | A message received more than once was sent at QoS < 2 at least once, and its outcome reports `qos_used < 2`. Messages sent only at QoS 2 are received at most once. |
| `InvRetainFidelity` | RETAIN on the wire always equals what was requested. It is never silently cleared. No modelled design clears RETAIN, so this is a regression guard. |
| `InvQoSFidelity` | A Delivered or Indeterminate outcome reports exactly the lowest QoS used on the wire, so every downgrade is reported. |
| `InvFairActionEnabled` | While some accepted message is unresolved, one of the fair actions is enabled: connect, task step, server receive, client ack or caller consume. This progress check is based on `ENABLED`. It backs up the liveness results. |

Liveness properties are checked under weak fairness of connect, task steps, server receive,
client ack and caller consume.

| Property | Meaning |
|---|---|
| `LiveResolved == []<>AllResolved` | Every accepted message eventually gets its outcome. |
| `LiveSettled == []<>Settled` | The queue and the session store eventually drain. There is no head-of-line blocking and no stranded stored message. |

Negative controls. Each must fail.

| Property | Why it must fail |
|---|---|
| `NegImpossible == []<>(nextM > N + 1)` | Unreachable. |
| `NegAllDelivered` | A message can legitimately end Indeterminate or Rejected. |
| `NegQuarantineReleased == []<>(quar = {})` | If the last connection has Session Present = 1, the quarantine is held forever by design. |
| `LiveResolved` under `SpecNoConnectFairness` | Without fairness on Connect the client may stay offline forever. |

## tla-mcp 0.9.4 caveat

The modelers found two liveness defects in tla-mcp 0.9.4:

- `P ~> Q` can pass vacuously.
- Missing fairness is not honoured: the checker behaves as if strong fairness were present.

To work around them:

- All liveness is written as `[]<>` over predicates that become stable. Nothing uses `~>`.
- Negative controls are included, and their results are recorded below.
- Every safety run includes `InvFairActionEnabled`.

`OfflineQueue_NEG_noconnectfair.cfg` is the probe for the fairness defect. Without fairness on
Connect, `LiveResolved` must fail. The tool reports `ok`, which reproduces the defect. Because of
this, the positive liveness results are evidence, not proof: they are only as strong as the
`[]<>` form, the negative controls and `InvFairActionEnabled` together.

A second tooling limit: the MCP client stops a `check_spec` call that is silent for 1800 s. Any
run that needed longer is listed as inconclusive and was split by dimension instead.

## Runs of the chosen design (final spec)

All runs use `K = 2` packet ids, `Design = "Fix"`, `Quarantine = TRUE`, `Report = "handle"` and
`ReplayDowngrade = FALSE`. Every run checks all 16 invariants listed above. Some runs shared the
machine, so the elapsed times include contention.

| cfg | N | MaxConns | RMSet | QSet | RSet | BSet | MQSet | RASet | MBSet | Result | States | Depth | Secs |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `OfflineQueue.cfg` (RETAIN / Retain Available dimension) | 3 | 3 | {1} | {1,2} | {T,F} | {F} | {2} | {T,F} | {T} | ok | 339,852 | 31 | 450 |
| `OfflineQueue_rm2.cfg` | 3 | 3 | {2} | {1,2} | {T,F} | {F} | {2} | {T,F} | {T} | ok | 589,643 | 33 | 799 |
| `OfflineQueue_qos.cfg` (Maximum QoS dimension, including downgrade to 0) | 3 | 3 | {1,2} | {1,2} | {F} | {F} | {0,1,2} | {T} | {T} | ok | 402,577 | 33 | 660 |
| `OfflineQueue_size.cfg` (Maximum Packet Size dimension) | 3 | 3 | {1} | {1,2} | {F} | {T,F} | {2} | {T} | {T,F} | ok | 339,852 | 31 | 468 |
| `OfflineQueue_alldims.cfg` (every dimension at once) | 2 | 2 | {1,2} | {1,2} | {T,F} | {T,F} | {0,1,2} | {T,F} | {T,F} | ok | 751,999 | 22 | 1323 |
| `OfflineQueue_live.cfg` (liveness: `LiveResolved`, `LiveSettled`) | 2 | 3 | {1,2} | {1,2} | {T,F} | {F} | {0,2} | {T,F} | {T} | ok | 157,270 | 26 | 303 |

These runs were not completed and are recorded as inconclusive, not as passes:

- The RETAIN dimension with `RMSet = {1,2}` at N=3, MaxConns=3 hit `limit_reached` at 935,089
  states (depth 23) after 1700 s, because of the 1800 s MCP limit. It is covered by the two runs
  split by Receive Maximum, plus `OfflineQueue_qos.cfg`, which mixes Receive Maximum 1 and 2
  across connections.
- An all-dimensions run at N=2, MaxConns=3 on an earlier revision of the spec was stopped by the
  MCP timeout.
- The Maximum Packet Size dimension with `RMSet = {2}` was not run. The chosen design treats
  Maximum Packet Size and Retain Available non-conformance through identical code paths, and the
  two `RMSet = {1}` runs produced the same state graph (339,852 states each).

## Current code (D0), variants and negative controls

The run results for this section are listed at the end of this file, in the section "D0,
variant and negative-control results (final spec)".

What D0 models. It follows the code at the time of modelling (`client/direct/replay.rs`,
`client/direct/mod.rs`):

- An offline publish returns `Ok(packet_id)` at once. It has no completion, so the caller
  believes it will be delivered.
- A flush that finds a non-conforming message drops it silently.
- A Maximum QoS downgrade is silent.
- Replay resends stored messages as they are, against the new CONNACK.
- Session Present = 0 discards the session state. Only live-publish futures learn of it, and
  they get Indeterminate.
- The replay task is tied to its connection only through a `Weak` writer, and it pops the
  shared queue by position (`pop_front`).
- `pop_front` runs before `store_unacked_publish`, which leaves a gap in which the packet id
  belongs neither to the queue nor to the store. The allocator does not see ids in that gap.

## Mapping from spec actions to client code

The names below are as of this modelling pass. The code is being reworked alongside.

| Spec | Client concept |
|---|---|
| `Accept` | `stage_publish` / `queue_publish_message`, with enqueue-time checks (`check_publish_size` plus a Retain Available check) and `allocate_packet_id`. In the chosen design the allocator also excludes quarantined ids and ids of messages being conformed. |
| `outcome`, `outQ`, `Resolve` (`Report = "handle"`) | The per-publish completion handle: Delivered{qos_used}, Rejected or Indeterminate. Today online publishes use `PublishAck` futures, and queued ones get `PublishResult::QoS1Or2 { packet_id }` with no completion. |
| `Report = "pidEvent"`, `CallerConsume` | The rejected alternative: outcomes reported through a packet-id-keyed callback or event stream. |
| `Connect` | `connect`: `apply_server_capabilities` (Session Present = 0 leads to `discard_session_state`), `reset_send_quota`, `advance_connection_epoch`, then spawning `SessionReplay`. In the chosen design Session Present = 0 requeues QoS 1 at the head of `OfflineQueue` and resolves QoS 2 as Indeterminate (PUBLISH stage) or Delivered (PUBREL stage). Connect also kills the previous connection's task and clears quarantine when Session Present = 0. |
| `Lose` | Transport or reader failure. |
| `ReplayStep`, `SendReplay`, `SendReplayRel`, `AbandonReplay` | `SessionReplay::replay_session_state` (`OutboundReplay::Publish` / `PubRel`), with a re-check of each stored PUBLISH against the new CONNACK. `AbandonReplay` resolves Indeterminate and quarantines a QoS 2 id. |
| `Conform`, `Drop`, `Claim` (`ClaimFix`) | `SessionReplay::flush_offline_queue`, `conform_queued` and `take_slot`. `ClaimFix` is the required atomic step: check the connection epoch, remove the message by identity, store it, then write it. |
| `ClaimD0`, `StoreStep`, `WriteStep` | Today's `pop_front`, then `store_unacked_publish`, then `write`. These steps are separate and are not bound to the connection. |
| `task[e]`, `e = conns` guards | The connection epoch that replay and flush work is bound to. |
| `quar` | New state: QoS 2 ids abandoned during replay, excluded from allocation until Session Present = 0. |
| `infl`, `caps.rm` | The `FlowControlManager` send quota (`claim_send_quota`, `acknowledge`). |
| `ServerRecv`, `ClientAck` | The broker, plus the client's PUBACK/PUBREC/PUBCOMP handling (`ack.rs`, `handlers.rs`, `release_outbound_quota`). |

## Abstractions and refinements outside the model

- **Live publishes.** The model sends every accepted publish, online or offline, through the same
  queue, per-publish outcome and flush step. A publish made while connected, with the flush idle,
  goes out immediately through that path. So the late design update ("live publishes get the
  same per-publish outcome; connection loss leaves the handle pending") is what the model
  already checks. `NoSilentLoss`, `LiveResolved` and the exactly-once resolution of `outcome`
  cover live publishes too.
- **QoS 2 PUBREL stage.** QoS 2 is split into PUBREC, PUBREL and PUBCOMP. A PUBREL-stage message
  at Session Present = 0 resolves Delivered{2}, which is the second late design update. At
  Session Present = 1 its PUBREL is replayed.
- **Server behaviour.** The server always accepts: there are no error reason codes. Two
  implementation refinements are therefore outside the model:
  - The mismatched-ack protocol-error rule: an ack whose type does not match the stored exchange
    is a protocol error.
  - "An error PUBREC removes state before reporting Rejected."

  Both refine how a server refusal ends an exchange. Neither adds a new way to deliver or lose a
  message.
- **RETAIN, oversize and Maximum QoS** are abstracted to one flag or level per message. Topic
  aliases, the payload and the DUP bit are not modelled. DUP is implied: a replay resends the
  same id.
- **Bounds.** The results are bounded model checking. They hold for the constants listed, not in
  general.

## D0, variant and negative-control results (final spec)

Each safety cfg checks `TypeOK` and one property. BFS returns the shortest counterexample.

| cfg | Expected | Result | States | Depth | What the counterexample shows |
|---|---|---|---|---|---|
| `OfflineQueue_D0_NoSilentLoss.cfg` | violation | `InvNoSilentLoss` violated | 457 | 6 | An offline publish is told Ok. After reconnecting with a smaller Maximum Packet Size, the flush drops it with no report. |
| `OfflineQueue_D0_RetainAvailable.cfg` | violation | `InvRetainAvailable` violated | 741 | 10 | A stored RETAIN message is replayed after a reconnect with Retain Available = 0. |
| `OfflineQueue_D0_MaximumPacketSize.cfg` | violation | `InvMaximumPacketSize` violated | 684 | 10 | A stored message is replayed after a reconnect with a smaller Maximum Packet Size. |
| `OfflineQueue_D0_MaximumQoS.cfg` | violation | `InvMaximumQoS` violated | 788 | 10 | A stored QoS 2 message is replayed at QoS 2 after a reconnect with Maximum QoS 1. |
| `OfflineQueue_D0_QoSFidelity.cfg` | violation | `InvQoSFidelity` violated | 289 | 8 | A QoS 2 publish is silently sent at QoS 1, while the caller was told Ok at QoS 2. |
| `OfflineQueue_D0_PidUnique.cfg` | violation | `InvPidUnique` violated | 70 | 7 | In the pop/store gap the allocator hands a new publish an id that is still in flight. |
| `OfflineQueue_D0_ExactlyOnce.cfg` | violation | `InvExactlyOnce` violated | 2,662 | 13 | A requested QoS 2 message is silently downgraded to QoS 1 and replayed, so it is delivered twice while reported as QoS 2. |
| `OfflineQueue_D0_Order.cfg` (MaxConns = 3) | violation | `InvOrder` violated | 21,739 | 20 | A stale task pops m1, but m1 is stored only after the next connection's replay snapshot. m2 is delivered first, and m1 arrives on the following reconnect. |
| `OfflineQueue_D0_NoStaleServerPid.cfg` | violation | `InvNoStaleServerPid` violated | 261 | 10 | The pop/store gap reuses the id of an in-flight QoS 2 message. The server would de-duplicate the new message away. |
| `OfflineQueue_D0_live.cfg` | liveness failure | `LiveSettled` violated | 37,703 | 30 | A message stored by a stale task misses the replay snapshot and stays in the store forever. |
| `OfflineQueue_V_noquarantine.cfg` | violation | `InvNoStaleServerPid` violated | 1,021 | 11 | Without quarantine, the id of a QoS 2 message abandoned during replay is reused while the server still holds its QoS 2 state. |
| `OfflineQueue_V_replaydowngrade.cfg` | violation | `InvNoStaleServerPid` violated | 200 | 10 | A replay downgrades a stored QoS 2 message to QoS 1 under the same id while the server holds QoS 2 state for it. |
| `OfflineQueue_V_pidevent.cfg` | violation | `InvNoMisattribution` violated | 409 | 8 | ABA misattribution: the Rejected event for id 1 (m1) is consumed after id 1 was reallocated to m2, so m2 is reported Rejected. |
| `OfflineQueue_NEG_impossible.cfg` | liveness failure | fails | 157,270 | 26 | The unreachable goal is correctly reported. |
| `OfflineQueue_NEG_alldelivered.cfg` | liveness failure | fails | 157,270 | 26 | A message ends Indeterminate. |
| `OfflineQueue_NEG_quarantine.cfg` | liveness failure | fails | 157,270 | 26 | A QoS 2 id is quarantined, and the final connection has Session Present = 1. |
| `OfflineQueue_NEG_noconnectfair.cfg` | liveness failure | **ok (tool defect)** | 157,270 | 26 | The missing fairness is not honoured, which reproduces the known tla-mcp 0.9.4 defect. |

The liveness runs of the chosen design (`OfflineQueue_live.cfg`) and all the negative controls
use the same constants.
