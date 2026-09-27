---------------------------- MODULE Qos1Backlog ----------------------------
(***************************************************************************)
(* Broker QoS 1 delivery under overload: bounded per-client delivery        *)
(* channel, outbound window, storage queue, Notify permit, and the          *)
(* handler-side backlog drain.                                              *)
(*                                                                          *)
(* There is no separately stored "backlog flag": a client is behind         *)
(* (Backlog(s)) exactly when its storage queue is non-empty, and the        *)
(* router's emptiness check, the router's append and the handler's limited  *)
(* take are all performed under the storage queue's own lock, so each is    *)
(* one atomic step here.  (Qos1BacklogLockFree shows why a flag that is     *)
(* cleared before the take breaks FIFO.)                                    *)
(*                                                                          *)
(* Every publisher publishes MaxMsgs messages, each routed to every         *)
(* subscriber at QoS 1.  The router routes a message for subscriber s:     *)
(*   - behind the backlog (append to storage) while Backlog(s) or while a   *)
(*     reconnect hand-off is in progress, so FIFO order is kept;            *)
(*   - else into the channel if it has room;                                *)
(*   - else it waits with a deadline; a timeout (possibly spurious, even    *)
(*     when the channel has room) queues to storage.                        *)
(* After every queue to storage the router posts a permit.                  *)
(* The handler consumes the channel only while its window has room (RM is  *)
(* min(client Receive Maximum, broker cap)), and drains storage only when   *)
(* the channel is empty, taking at most Min(free window, Batch) from the    *)
(* FRONT of storage; acks are read only between batches.  Every trigger --  *)
(* (i) the channel running empty, (ii) an ack freeing a slot, (iii) a       *)
(* router queue, (iv) a finished batch with storage left, (v) the end of a  *)
(* hand-off -- posts the permit; the drain runs only from the Notify arm.   *)
(*                                                                          *)
(* Purge: the expiry sweep may remove any queued message at any time.       *)
(* Takeover (two phases): TakeoverBegin -- the new handler registers; the   *)
(* old handler's unacked in-flight (send order), unsent batch and channel   *)
(* are in hand-off and every router routes behind; the new handler's        *)
(* channel arm and drain wait.  TakeoverEnd -- the old handler has          *)
(* re-queued them at the FRONT of storage; the new handler gets a permit.   *)
(* CleanTakeover: a clean-start reconnect discards everything queued,       *)
(* held, in the channel and in flight (MQTT-3.1.2-4) in one step.          *)
(*                                                                          *)
(* CONSTANT NotifyTrigger selects whether the router posts a permit after   *)
(* queueing (trigger iii):                                                  *)
(*   FALSE -- a timeout that raced an already-empty channel leaves storage  *)
(*        non-empty with nothing to wake the drain.  Expected:             *)
(*        InvBacklogArmed and Live VIOLATED.                                *)
(*   TRUE  -- the permit wakes the drain.  Expected: holds.                 *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    NPubs,          \* publishers
    NSubs,          \* subscribers, each subscribed to everything at QoS 1
    MaxMsgs,        \* messages per publisher
    Cap,            \* delivery channel capacity (client_channel_capacity)
    RM,             \* outbound window: min(client Receive Maximum, max_outbound_inflight)
    Batch,          \* messages written per drain invocation
    MaxTakeovers,   \* reconnects allowed per subscriber (state-space bound)
    MaxPurges,      \* expiry purges allowed per subscriber (state-space bound)
    NotifyTrigger   \* TRUE = the router posts a permit after every queue

Pubs == 1..NPubs
Subs == 1..NSubs
Msgs == [p : Pubs, n : 1..MaxMsgs]

VARIABLES
    pubSent,        \* per publisher: messages published so far
    targets,        \* per publisher: subscribers the current message is still being routed to
    chan,           \* per subscriber: bounded delivery channel (FIFO)
    storage,        \* per subscriber: storage queue (FIFO)
    held,           \* per subscriber: batch taken from storage by the running drain (FIFO)
    draining,       \* per subscriber: a drain batch is between take and finish
    permit,         \* per subscriber: stored Notify permit
    drainPending,   \* per subscriber: the Notify arm fired and the drain has not run yet
    inflight,       \* per subscriber: sent, awaiting PUBACK
    delivered,      \* per subscriber: acked
    purged,         \* per subscriber: removed from storage by the expiry sweep
    discarded,      \* per subscriber: dropped by a clean-start reconnect
    sent,           \* per subscriber: first-send order to the transport (a re-send does not change it)
    handoff,        \* per subscriber: a reconnect hand-off is in progress
    oldChan,        \* per subscriber: what the displaced handler still has to re-queue (age order)
    takeovers       \* per subscriber: reconnects so far

vars == <<pubSent, targets, chan, storage, held, draining, permit, drainPending, inflight, delivered, purged, discarded, sent, handoff, oldChan, takeovers>>
subVars == <<chan, storage, held, draining, permit, drainPending, inflight, delivered, purged, discarded, sent, handoff, oldChan, takeovers>>

Range(seq) == {seq[i] : i \in DOMAIN seq}
Min(a, b) == IF a < b THEN a ELSE b
Free(s) == RM - Cardinality(inflight[s])
Backlog(s) == storage[s] /= <<>>
Behind(s) == Backlog(s) \/ handoff[s]
RemoveAt(seq, i) == SubSeq(seq, 1, i - 1) \o SubSeq(seq, i + 1, Len(seq))
FirstSend(seq, m) == IF m \in Range(seq) THEN seq ELSE Append(seq, m)

TypeOK ==
    /\ pubSent \in [Pubs -> 0..MaxMsgs]
    /\ targets \in [Pubs -> SUBSET Subs]
    /\ chan \in [Subs -> Seq(Msgs)]
    /\ storage \in [Subs -> Seq(Msgs)]
    /\ held \in [Subs -> Seq(Msgs)]
    /\ draining \in [Subs -> BOOLEAN]
    /\ permit \in [Subs -> BOOLEAN]
    /\ drainPending \in [Subs -> BOOLEAN]
    /\ inflight \in [Subs -> SUBSET Msgs]
    /\ delivered \in [Subs -> SUBSET Msgs]
    /\ purged \in [Subs -> SUBSET Msgs]
    /\ discarded \in [Subs -> SUBSET Msgs]
    /\ sent \in [Subs -> Seq(Msgs)]
    /\ handoff \in [Subs -> BOOLEAN]
    /\ oldChan \in [Subs -> Seq(Msgs)]
    /\ takeovers \in [Subs -> 0..MaxTakeovers]

Init ==
    /\ pubSent = [p \in Pubs |-> 0]
    /\ targets = [p \in Pubs |-> {}]
    /\ chan = [s \in Subs |-> <<>>]
    /\ storage = [s \in Subs |-> <<>>]
    /\ held = [s \in Subs |-> <<>>]
    /\ draining = [s \in Subs |-> FALSE]
    /\ permit = [s \in Subs |-> FALSE]
    /\ drainPending = [s \in Subs |-> FALSE]
    /\ inflight = [s \in Subs |-> {}]
    /\ delivered = [s \in Subs |-> {}]
    /\ purged = [s \in Subs |-> {}]
    /\ discarded = [s \in Subs |-> {}]
    /\ sent = [s \in Subs |-> <<>>]
    /\ handoff = [s \in Subs |-> FALSE]
    /\ oldChan = [s \in Subs |-> <<>>]
    /\ takeovers = [s \in Subs |-> 0]

Cur(p) == [p |-> p, n |-> pubSent[p]]

(* Publisher handler: a PUBLISH arrives; PUBACK is withheld until targets = {} *)
PubStart(p) ==
    /\ targets[p] = {}
    /\ pubSent[p] < MaxMsgs
    /\ pubSent' = [pubSent EXCEPT ![p] = @ + 1]
    /\ targets' = [targets EXCEPT ![p] = Subs]
    /\ UNCHANGED subVars

(* Router: append to storage under the storage lock, then post a permit *)
QueueBehind(p, s) ==
    /\ storage' = [storage EXCEPT ![s] = Append(@, Cur(p))]
    /\ permit' = [permit EXCEPT ![s] = @ \/ NotifyTrigger]
    /\ targets' = [targets EXCEPT ![p] = @ \ {s}]
    /\ UNCHANGED <<pubSent, chan, held, draining, drainPending, inflight, delivered, purged, discarded, sent, handoff, oldChan, takeovers>>

(* Router: storage non-empty or hand-off in progress -- go behind without waiting *)
RouteBehind(p, s) ==
    /\ s \in targets[p]
    /\ Behind(s)
    /\ QueueBehind(p, s)

(* Router: not behind, channel has room *)
RouteEnqueue(p, s) ==
    /\ s \in targets[p]
    /\ ~Behind(s)
    /\ Len(chan[s]) < Cap
    /\ chan' = [chan EXCEPT ![s] = Append(@, Cur(p))]
    /\ targets' = [targets EXCEPT ![p] = @ \ {s}]
    /\ UNCHANGED <<pubSent, storage, held, draining, permit, drainPending, inflight, delivered, purged, discarded, sent, handoff, oldChan, takeovers>>

(* Router: not behind, deadline elapsed -- possibly spuriously *)
RouteTimeout(p, s) ==
    /\ s \in targets[p]
    /\ ~Behind(s)
    /\ QueueBehind(p, s)

(* Subscriber handler: channel arm, polled only while the window has room and no hand-off is pending *)
Consume(s) ==
    /\ chan[s] /= <<>>
    /\ ~draining[s]
    /\ ~handoff[s]
    /\ Free(s) > 0
    /\ LET m == Head(chan[s])
           rest == Tail(chan[s])
       IN  /\ chan' = [chan EXCEPT ![s] = rest]
           /\ inflight' = [inflight EXCEPT ![s] = @ \cup {m}]
           /\ sent' = [sent EXCEPT ![s] = FirstSend(@, m)]
           /\ permit' = [permit EXCEPT ![s] = @ \/ (rest = <<>> /\ Backlog(s))]
    /\ UNCHANGED <<pubSent, targets, storage, held, draining, drainPending, delivered, purged, discarded, handoff, oldChan, takeovers>>

(* Subscriber handler: the Notify arm fires -- the only entry into the drain *)
Notified(s) ==
    /\ permit[s]
    /\ ~draining[s]
    /\ ~handoff[s]
    /\ permit' = [permit EXCEPT ![s] = FALSE]
    /\ drainPending' = [drainPending EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, draining, inflight, delivered, purged, discarded, sent, handoff, oldChan, takeovers>>

(* drain_backlog: storage empty -- nothing to do *)
DrainIdle(s) ==
    /\ drainPending[s]
    /\ ~draining[s]
    /\ ~Backlog(s)
    /\ drainPending' = [drainPending EXCEPT ![s] = FALSE]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, draining, permit, inflight, delivered, purged, discarded, sent, handoff, oldChan, takeovers>>

(* drain_backlog: the channel is older than storage, or the window is full -- come back later *)
DrainDefer(s) ==
    /\ drainPending[s]
    /\ ~draining[s]
    /\ Backlog(s)
    /\ chan[s] /= <<>> \/ Free(s) = 0
    /\ drainPending' = [drainPending EXCEPT ![s] = FALSE]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, draining, permit, inflight, delivered, purged, discarded, sent, handoff, oldChan, takeovers>>

(* drain_backlog: take at most Min(free window, Batch) from the front of storage, under the storage lock *)
DrainBegin(s) ==
    /\ drainPending[s]
    /\ ~draining[s]
    /\ Backlog(s)
    /\ chan[s] = <<>>
    /\ Free(s) > 0
    /\ LET k == Min(Len(storage[s]), Min(Free(s), Batch))
       IN  /\ held' = [held EXCEPT ![s] = SubSeq(storage[s], 1, k)]
           /\ storage' = [storage EXCEPT ![s] = SubSeq(storage[s], k + 1, Len(storage[s]))]
    /\ draining' = [draining EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<pubSent, targets, chan, permit, drainPending, inflight, delivered, purged, discarded, sent, handoff, oldChan, takeovers>>

(* drain_backlog: send the next held message *)
DrainSend(s) ==
    /\ draining[s]
    /\ held[s] /= <<>>
    /\ LET m == Head(held[s])
       IN  /\ held' = [held EXCEPT ![s] = Tail(@)]
           /\ inflight' = [inflight EXCEPT ![s] = @ \cup {m}]
           /\ sent' = [sent EXCEPT ![s] = FirstSend(@, m)]
    /\ UNCHANGED <<pubSent, targets, chan, storage, draining, permit, drainPending, delivered, purged, discarded, handoff, oldChan, takeovers>>

(* drain_backlog: batch done -- return to select; re-arm through the permit if storage is still non-empty *)
DrainFinish(s) ==
    /\ draining[s]
    /\ held[s] = <<>>
    /\ draining' = [draining EXCEPT ![s] = FALSE]
    /\ drainPending' = [drainPending EXCEPT ![s] = FALSE]
    /\ permit' = [permit EXCEPT ![s] = @ \/ Backlog(s)]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, inflight, delivered, purged, discarded, sent, handoff, oldChan, takeovers>>

(* Subscriber PUBACK is read -- only between batches, only on the live connection *)
Ack(s, m) ==
    /\ m \in inflight[s]
    /\ ~draining[s]
    /\ ~handoff[s]
    /\ inflight' = [inflight EXCEPT ![s] = @ \ {m}]
    /\ delivered' = [delivered EXCEPT ![s] = @ \cup {m}]
    /\ permit' = [permit EXCEPT ![s] = @ \/ Backlog(s)]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, draining, drainPending, purged, discarded, sent, handoff, oldChan, takeovers>>

(* Expiry sweep removes one queued message; the count is the queue length, so nothing else changes *)
Purge(s) ==
    /\ Cardinality(purged[s]) < MaxPurges
    /\ \E i \in DOMAIN storage[s] :
        /\ purged' = [purged EXCEPT ![s] = @ \cup {storage[s][i]}]
        /\ storage' = [storage EXCEPT ![s] = RemoveAt(@, i)]
    /\ UNCHANGED <<pubSent, targets, chan, held, draining, permit, drainPending, inflight, delivered, discarded, sent, handoff, oldChan, takeovers>>

(* Reconnect, phase 1: the new handler registers; the old one's custody enters the hand-off *)
TakeoverBegin(s) ==
    /\ takeovers[s] < MaxTakeovers
    /\ ~draining[s]
    /\ ~handoff[s]
    /\ LET unacked == SelectSeq(sent[s], LAMBDA m : m \in inflight[s])
       IN  oldChan' = [oldChan EXCEPT ![s] = unacked \o held[s] \o chan[s]]
    /\ inflight' = [inflight EXCEPT ![s] = {}]
    /\ held' = [held EXCEPT ![s] = <<>>]
    /\ chan' = [chan EXCEPT ![s] = <<>>]
    /\ drainPending' = [drainPending EXCEPT ![s] = FALSE]
    /\ permit' = [permit EXCEPT ![s] = FALSE]
    /\ handoff' = [handoff EXCEPT ![s] = TRUE]
    /\ takeovers' = [takeovers EXCEPT ![s] = @ + 1]
    /\ UNCHANGED <<pubSent, targets, storage, draining, delivered, purged, discarded, sent>>

(* Reconnect, phase 2: the old handler re-queued its custody at the front; the new handler gets a permit *)
TakeoverEnd(s) ==
    /\ handoff[s]
    /\ storage' = [storage EXCEPT ![s] = oldChan[s] \o @]
    /\ oldChan' = [oldChan EXCEPT ![s] = <<>>]
    /\ handoff' = [handoff EXCEPT ![s] = FALSE]
    /\ permit' = [permit EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<pubSent, targets, chan, held, draining, drainPending, inflight, delivered, purged, discarded, sent, takeovers>>

(* Clean-start reconnect: everything queued, held, in the channel and in flight is discarded in one step *)
CleanTakeover(s) ==
    /\ takeovers[s] < MaxTakeovers
    /\ ~draining[s]
    /\ ~handoff[s]
    /\ discarded' = [discarded EXCEPT ![s] = @ \cup Range(storage[s]) \cup Range(chan[s]) \cup Range(held[s]) \cup inflight[s]]
    /\ storage' = [storage EXCEPT ![s] = <<>>]
    /\ chan' = [chan EXCEPT ![s] = <<>>]
    /\ held' = [held EXCEPT ![s] = <<>>]
    /\ inflight' = [inflight EXCEPT ![s] = {}]
    /\ drainPending' = [drainPending EXCEPT ![s] = FALSE]
    /\ permit' = [permit EXCEPT ![s] = TRUE]
    /\ takeovers' = [takeovers EXCEPT ![s] = @ + 1]
    /\ UNCHANGED <<pubSent, targets, draining, delivered, purged, sent, handoff, oldChan>>

Next ==
    \/ \E p \in Pubs : PubStart(p)
    \/ \E p \in Pubs, s \in Subs : RouteBehind(p, s) \/ RouteEnqueue(p, s) \/ RouteTimeout(p, s)
    \/ \E s \in Subs :
        \/ Consume(s) \/ Notified(s)
        \/ DrainIdle(s) \/ DrainDefer(s) \/ DrainBegin(s) \/ DrainSend(s) \/ DrainFinish(s)
        \/ Purge(s) \/ TakeoverBegin(s) \/ TakeoverEnd(s) \/ CleanTakeover(s)
    \/ \E s \in Subs, m \in Msgs : Ack(s, m)

Fairness ==
    /\ \A p \in Pubs : WF_vars(PubStart(p))
    /\ \A p \in Pubs, s \in Subs : WF_vars(RouteBehind(p, s)) /\ WF_vars(RouteEnqueue(p, s))
    /\ \A s \in Subs :
        /\ WF_vars(Consume(s)) /\ WF_vars(Notified(s)) /\ WF_vars(TakeoverEnd(s))
        /\ WF_vars(DrainIdle(s)) /\ WF_vars(DrainDefer(s)) /\ WF_vars(DrainBegin(s))
        /\ WF_vars(DrainSend(s)) /\ WF_vars(DrainFinish(s))
    /\ \A s \in Subs, m \in Msgs : WF_vars(Ack(s, m))

Spec == Init /\ [][Next]_vars /\ Fairness

(* A message is routed to s once the router has finished with it for s *)
Routed(s) ==
    {m \in Msgs :
        /\ m.n <= pubSent[m.p]
        /\ ~(m.n = pubSent[m.p] /\ s \in targets[m.p])}

Pending(s) == Range(chan[s]) \cup Range(storage[s]) \cup Range(held[s]) \cup Range(oldChan[s]) \cup inflight[s]
Custody(s) == Pending(s) \cup delivered[s] \cup purged[s] \cup discarded[s]

InvChanBound == \A s \in Subs : Len(chan[s]) <= Cap
InvWindowBound == \A s \in Subs : Cardinality(inflight[s]) <= RM
InvBatchBound == \A s \in Subs : Len(held[s]) <= Batch
InvNoLoss == \A s \in Subs : Routed(s) = Custody(s)
InvNoDuplicate == \A s \in Subs :
    /\ Len(chan[s]) = Cardinality(Range(chan[s]))
    /\ Len(storage[s]) = Cardinality(Range(storage[s]))
    /\ Len(held[s]) = Cardinality(Range(held[s]))
    /\ Len(oldChan[s]) = Cardinality(Range(oldChan[s]))
    /\ Len(sent[s]) = Cardinality(Range(sent[s]))
    /\ Cardinality(Pending(s)) = Len(chan[s]) + Len(storage[s]) + Len(held[s]) + Len(oldChan[s]) + Cardinality(inflight[s])
    /\ (delivered[s] \cup purged[s] \cup discarded[s]) \cap Pending(s) = {}
    /\ delivered[s] \cap purged[s] = {}
    /\ delivered[s] \cap discarded[s] = {}
    /\ purged[s] \cap discarded[s] = {}
(* Something will still wake the drain: a permit, a pending trigger, a channel message (i), an ack to come (ii) or the end of a hand-off (v) *)
InvBacklogArmed == \A s \in Subs :
    (Backlog(s) /\ ~draining[s]) => (permit[s] \/ drainPending[s] \/ chan[s] /= <<>> \/ inflight[s] /= {} \/ handoff[s])
InvFifo == \A s \in Subs : \A i, j \in DOMAIN sent[s] :
    (i < j /\ sent[s][i].p = sent[s][j].p) => sent[s][i].n < sent[s][j].n

AllDelivered == \A s \in Subs : delivered[s] \cup purged[s] \cup discarded[s] = Msgs
Live == <>AllDelivered

=============================================================================
