# QoS 1 backlog drain — TLA+ verification

Model of the broker's QoS 1 delivery path under overload: bounded per-client
delivery channel, outbound window, storage queue, Notify permit, and the
handler-side `drain_backlog`, including expiry purges and reconnect takeover.

Observed failure that motivated it: 8 QoS 1 publishers saturating 8 QoS 1
subscribers; after ~8 s delivery stalls to zero while every client stays
connected, broker RSS 9 MB → 15 GB.

## Modules

| Module / cfg | What it models | Expected |
|---|---|---|
| `Qos1Backlog.tla` + `Qos1Backlog.cfg` | The design: a client is behind iff its storage queue is non-empty or a reconnect hand-off is in progress; the router's check, append and the handler's window-limited FIFO take are atomic w.r.t. each other (storage lock); channel consumed only while the window has room; drain only when the channel is empty, at most `Batch` per invocation, acks read only between batches; every trigger posts the Notify permit and the drain runs only from the Notify arm; expiry purge; two-phase takeover (routers route behind during the hand-off, the displaced handler re-queues unacked in-flight + unsent batch + channel at the front, the new handler starts with a permit); clean-start discard | holds |
| `Qos1Backlog_nonotify.cfg` | Same, but the router posts no permit after queueing | `InvBacklogArmed` violated |
| `Qos1BacklogLockFree.tla` + `.cfg` | Rejected variant: a separately stored backlog flag, cleared before the take and re-armed on leftover | `InvFifo` violated |

### Actions ↔ code

| Action | Code path |
|---|---|
| `PubStart` | publisher's PUBLISH arrives; PUBACK withheld until `targets = {}` |
| `RouteBehind` | router: `queue.count() > 0 \|\| queue.handoff` → `queue.push` + `queue.notify()` |
| `RouteEnqueue` | router: `try_send` succeeds |
| `RouteTimeout` | router: timed `reserve()` elapsed or closed (possibly spuriously) → push + notify |
| `Consume` | handler `qos1_rx` arm, polled only while `outbound_inflight < W`, ≤ `Batch` per poll, pending during a hand-off |
| `Notified` | `notified()` arm — the only entry into `drain_backlog` |
| `DrainIdle` / `DrainDefer` | `drain_backlog` steps 1–3: storage empty / channel not yet empty or window full |
| `DrainBegin` | `queue.take(min(free, Batch))`, FIFO, one critical section |
| `DrainSend` / `DrainFinish` | send the batch; if storage is left, `queue.notify()` and return to select |
| `Ack` | PUBACK read between batches → `queue.notify()` |
| `Purge` | `cleanup_expired` / expiry-on-take (count is the queue length, nothing else changes) |
| `TakeoverBegin` | `register_client` sets `handoff` before inserting the new entry; new handler's lane and drain pending |
| `TakeoverEnd` | displaced handler's `requeue_front(unacked ++ held ++ channel)`, then `released` and a permit |
| `CleanTakeover` | clean-start discard at the hand-off point |

Invariants: `TypeOK`, `InvChanBound`, `InvWindowBound`, `InvBatchBound`,
`InvNoLoss` (routed = channel ∪ storage ∪ held ∪ hand-off ∪ inflight ∪
delivered ∪ purged ∪ discarded), `InvNoDuplicate`, `InvBacklogArmed`
(non-empty storage always has a permit, a pending drain, a channel message to
consume, an ack to come, or a hand-off about to end), `InvFifo` (per
publisher, the first-send order to each subscriber is publish order; a re-send
after a takeover does not change it). Liveness: `Live == <>AllDelivered`
(delivered ∪ purged ∪ discarded) under weak fairness.

## Results (2026-09-07, v5)

| Module / cfg | constants | result |
|---|---|---|
| `Qos1Backlog.cfg` (shipped) | 1p×2s×2m Cap1 RM1 Batch1, 1 takeover, 1 purge | ok, 98 422 states, safety + Live |
| `Qos1Backlog` | 2p×1s×2m, same bounds | ok, 17 839 states, safety + Live |
| `Qos1Backlog` | 2p×2s×1m, same bounds | ok, 272 113 states, safety + Live |
| `Qos1Backlog` | 2p×1s×3m Cap1 RM2 Batch1 | ok, 603 547 states, safety + Live |
| `Qos1Backlog` | 1p×2s×3m Cap1 RM3 Batch1, no takeover, no purge | ok, 23 590 states, safety + Live |
| `Qos1Backlog` | 1p×2s×3m Cap2 RM2 Batch2, no takeover, no purge | ok, 23 978 states, safety + Live |
| `Qos1Backlog` | 2p×2s×2m Cap1 RM1 Batch1, no takeover, no purge, safety only | ok, 913 110 states, 336 s |
| `Qos1Backlog_nonotify.cfg` | 1p×2s×2m | **`InvBacklogArmed` violated** in 2 steps |
| `Qos1BacklogLockFree.cfg` | 2p×1s×2m | **`InvFifo` violated** in 17 steps |

Larger shapes with takeover and purge enabled (2p×2s×2m) exceed a 10-minute
budget and are inconclusive; the bounds `MaxTakeovers` / `MaxPurges` exist to
keep the checked configurations exhaustive.

### Counterexample 1 — no router permit

`PubStart → RouteTimeout`: the deadline fires while the subscriber's channel
and window are both empty. The message is queued, but the two remaining
triggers (channel running empty, an ack) can never fire again.

### Counterexample 2 — stored flag cleared before the take

Publisher 2's first message is queued behind the backlog. The drain clears the
flag, publisher 2 reads the cleared flag and enqueues its **second** message
into the channel, the window-limited take takes only an older message from
another publisher and re-arms the flag with publisher 2's first message still
in storage; the channel message is then written first:
`sent = << m(1,1), m(2,2), m(1,2) >>`. Hence: no stored flag; the decision
reads the storage queue itself under its lock, and the take is atomic with
that read.

### Design decisions the model forced

- The backlog condition is the storage queue's own emptiness (plus the
  hand-off flag), read under the storage lock; never a separately stored
  boolean.
- Take at most `min(free window, Batch)` from the front of storage,
  atomically; never re-queue at the tail; re-queue at the front only on a
  hand-off or a transport error.
- Post the Notify permit after every queue to storage and after every batch
  that leaves storage non-empty; run the drain only from the Notify arm.
- Consume the channel only while the window has room, so window-full
  propagates to channel-full and the publisher's timed wait is real
  backpressure. The window is `min(client Receive Maximum, broker cap)`.
- During a reconnect hand-off routers route behind, so nothing newer reaches
  the new channel before the displaced handler's custody reaches the front of
  storage.
- Acks are read only between batches, so batches must be bounded.

Verified for these constants only; not a proof for all sizes. Not modelled:
the QoS 0 lane, transport errors, the router lock order, packet ids,
scheduling and time budgets, the per-client queued cap, the on-disk format,
bridge ingress, broker restart.
