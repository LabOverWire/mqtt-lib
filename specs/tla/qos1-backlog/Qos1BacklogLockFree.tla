---------------------------- MODULE Qos1BacklogLockFree ----------------------------
(***************************************************************************)
(* Broker QoS 1 delivery under overload: bounded per-client delivery        *)
(* channel, Receive-Maximum window, storage queue, backlog flag, Notify     *)
(* permit, and the handler-side backlog drain.  Lock-free flag: the router  *)
(* and the handler never share a lock on the hot path.                      *)
(*                                                                          *)
(* Every publisher publishes MaxMsgs messages, each routed to every         *)
(* subscriber at QoS 1.  The router routes a message for subscriber s:     *)
(*   - behind the backlog (append to storage) while s's flag is set, so     *)
(*     an episode of overflow keeps FIFO order;                             *)
(*   - else into the channel if it has room;                                *)
(*   - else it waits with a deadline; a timeout (possibly spurious, even    *)
(*     when the channel has room) queues to storage.                        *)
(* After EVERY queue to storage the router sets the flag and posts a        *)
(* permit, whatever the flag was -- the handler may have cleared it since   *)
(* the router read it.                                                      *)
(* The handler consumes the channel only while its window has room (the    *)
(* Receive-Maximum gate), and drains storage only when the channel is       *)
(* empty: it CLEARS the flag first, then TAKES at most the free window      *)
(* from the front of storage in one atomic storage operation, re-arming    *)
(* the flag if anything remains; the router may append between the clear   *)
(* and the take.  Triggers: (i) the channel running empty, (ii) an ack      *)
(* freeing a slot, (iii) the Notify permit.  A drain loops while the flag   *)
(* is armed when its batch finishes.                                        *)
(*                                                                          *)
(* CONSTANT NotifyTrigger selects whether trigger (iii) exists:             *)
(*   FALSE -- a timeout that raced an already-empty channel leaves storage  *)
(*        non-empty with nothing to wake the drain.  Expected: Live        *)
(*        VIOLATED.                                                         *)
(*   TRUE  -- the permit wakes the drain.  Expected: holds.                 *)
(*                                                                          *)
(* CONSTANT ClearFirst selects when the drain clears the flag:              *)
(*   FALSE -- cleared unconditionally when the batch is done.  A message    *)
(*        the router queued behind the backlog during the batch is         *)
(*        orphaned.  Expected: InvBacklogArmed VIOLATED.                    *)
(*   TRUE  -- cleared before the take, re-armed if storage is left over.    *)
(*        Expected: holds.                                                  *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    NPubs,          \* publishers
    NSubs,          \* subscribers, each subscribed to everything at QoS 1
    MaxMsgs,        \* messages per publisher
    Cap,            \* delivery channel capacity (client_channel_capacity)
    RM,             \* subscriber Receive Maximum (outbound in-flight window)
    NotifyTrigger,  \* TRUE = drain has the Notify select branch
    ClearFirst      \* TRUE = flag cleared before the take, re-armed on leftover

Pubs == 1..NPubs
Subs == 1..NSubs
Msgs == [p : Pubs, n : 1..MaxMsgs]
Phases == {"idle", "take", "send"}

VARIABLES
    pubSent,        \* per publisher: messages published so far
    targets,        \* per publisher: subscribers the current message is still being routed to
    chan,           \* per subscriber: bounded delivery channel (FIFO)
    storage,        \* per subscriber: storage queue (FIFO)
    held,           \* per subscriber: batch taken from storage by the running drain (FIFO)
    phase,          \* per subscriber: drain phase
    backlog,        \* per subscriber: the AtomicBool flag
    permit,         \* per subscriber: stored Notify permit
    drainPending,   \* per subscriber: a drain trigger has fired and not yet been serviced
    inflight,       \* per subscriber: sent, awaiting PUBACK
    delivered,      \* per subscriber: acked
    sent            \* per subscriber: every message in the order it was written to the transport

vars == <<pubSent, targets, chan, storage, held, phase, backlog, permit, drainPending, inflight, delivered, sent>>
subVars == <<chan, storage, held, phase, backlog, permit, drainPending, inflight, delivered, sent>>

Range(seq) == {seq[i] : i \in DOMAIN seq}
Min(a, b) == IF a < b THEN a ELSE b
Free(s) == RM - Cardinality(inflight[s])

TypeOK ==
    /\ pubSent \in [Pubs -> 0..MaxMsgs]
    /\ targets \in [Pubs -> SUBSET Subs]
    /\ chan \in [Subs -> Seq(Msgs)]
    /\ storage \in [Subs -> Seq(Msgs)]
    /\ held \in [Subs -> Seq(Msgs)]
    /\ phase \in [Subs -> Phases]
    /\ backlog \in [Subs -> BOOLEAN]
    /\ permit \in [Subs -> BOOLEAN]
    /\ drainPending \in [Subs -> BOOLEAN]
    /\ inflight \in [Subs -> SUBSET Msgs]
    /\ delivered \in [Subs -> SUBSET Msgs]
    /\ sent \in [Subs -> Seq(Msgs)]

Init ==
    /\ pubSent = [p \in Pubs |-> 0]
    /\ targets = [p \in Pubs |-> {}]
    /\ chan = [s \in Subs |-> <<>>]
    /\ storage = [s \in Subs |-> <<>>]
    /\ held = [s \in Subs |-> <<>>]
    /\ phase = [s \in Subs |-> "idle"]
    /\ backlog = [s \in Subs |-> FALSE]
    /\ permit = [s \in Subs |-> FALSE]
    /\ drainPending = [s \in Subs |-> FALSE]
    /\ inflight = [s \in Subs |-> {}]
    /\ delivered = [s \in Subs |-> {}]
    /\ sent = [s \in Subs |-> <<>>]

Cur(p) == [p |-> p, n |-> pubSent[p]]

(* Publisher handler: a PUBLISH arrives; PUBACK is withheld until targets = {} *)
PubStart(p) ==
    /\ targets[p] = {}
    /\ pubSent[p] < MaxMsgs
    /\ pubSent' = [pubSent EXCEPT ![p] = @ + 1]
    /\ targets' = [targets EXCEPT ![p] = Subs]
    /\ UNCHANGED subVars

(* Router: queue to storage, then set the flag and post a permit, whatever the flag was *)
QueueBehind(p, s) ==
    /\ storage' = [storage EXCEPT ![s] = Append(@, Cur(p))]
    /\ backlog' = [backlog EXCEPT ![s] = TRUE]
    /\ permit' = [permit EXCEPT ![s] = @ \/ NotifyTrigger]
    /\ targets' = [targets EXCEPT ![p] = @ \ {s}]
    /\ UNCHANGED <<pubSent, chan, held, phase, drainPending, inflight, delivered, sent>>

(* Router: read the flag set -- go behind the backlog without waiting *)
RouteBehind(p, s) ==
    /\ s \in targets[p]
    /\ backlog[s]
    /\ QueueBehind(p, s)

(* Router: flag clear, channel has room *)
RouteEnqueue(p, s) ==
    /\ s \in targets[p]
    /\ ~backlog[s]
    /\ Len(chan[s]) < Cap
    /\ chan' = [chan EXCEPT ![s] = Append(@, Cur(p))]
    /\ targets' = [targets EXCEPT ![p] = @ \ {s}]
    /\ UNCHANGED <<pubSent, storage, held, phase, backlog, permit, drainPending, inflight, delivered, sent>>

(* Router: flag clear, deadline elapsed -- possibly spuriously *)
RouteTimeout(p, s) ==
    /\ s \in targets[p]
    /\ ~backlog[s]
    /\ QueueBehind(p, s)

(* Subscriber handler: channel arm, polled only while the window has room *)
Consume(s) ==
    /\ chan[s] /= <<>>
    /\ phase[s] = "idle"
    /\ Free(s) > 0
    /\ LET m == Head(chan[s])
           rest == Tail(chan[s])
       IN  /\ chan' = [chan EXCEPT ![s] = rest]
           /\ inflight' = [inflight EXCEPT ![s] = @ \cup {m}]
           /\ sent' = [sent EXCEPT ![s] = Append(@, m)]
           /\ drainPending' = [drainPending EXCEPT ![s] = @ \/ (rest = <<>> /\ backlog[s])]
    /\ UNCHANGED <<pubSent, targets, storage, held, phase, backlog, permit, delivered>>

(* Subscriber handler: the Notify select branch fires *)
Notified(s) ==
    /\ permit[s]
    /\ phase[s] = "idle"
    /\ permit' = [permit EXCEPT ![s] = FALSE]
    /\ drainPending' = [drainPending EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, phase, backlog, inflight, delivered, sent>>

(* drain_backlog: flag clear -- nothing to do *)
DrainIdle(s) ==
    /\ drainPending[s]
    /\ phase[s] = "idle"
    /\ ~backlog[s]
    /\ drainPending' = [drainPending EXCEPT ![s] = FALSE]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, phase, backlog, permit, inflight, delivered, sent>>

(* drain_backlog: the channel is older than storage, or the window is full -- come back later *)
DrainDefer(s) ==
    /\ drainPending[s]
    /\ phase[s] = "idle"
    /\ backlog[s]
    /\ chan[s] /= <<>> \/ Free(s) = 0
    /\ drainPending' = [drainPending EXCEPT ![s] = FALSE]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, phase, backlog, permit, inflight, delivered, sent>>

(* drain_backlog: clear the flag before touching storage *)
DrainClear(s) ==
    /\ drainPending[s]
    /\ phase[s] = "idle"
    /\ backlog[s]
    /\ chan[s] = <<>>
    /\ Free(s) > 0
    /\ backlog' = [backlog EXCEPT ![s] = IF ClearFirst THEN FALSE ELSE @]
    /\ phase' = [phase EXCEPT ![s] = "take"]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, permit, drainPending, inflight, delivered, sent>>

(* drain_backlog: atomic take of at most the free window from the front of storage; re-arm on leftover *)
DrainTake(s) ==
    /\ phase[s] = "take"
    /\ LET k == Min(Len(storage[s]), Free(s))
           rest == SubSeq(storage[s], k + 1, Len(storage[s]))
       IN  /\ held' = [held EXCEPT ![s] = SubSeq(storage[s], 1, k)]
           /\ storage' = [storage EXCEPT ![s] = rest]
           /\ backlog' = [backlog EXCEPT ![s] = @ \/ (ClearFirst /\ rest /= <<>>)]
    /\ phase' = [phase EXCEPT ![s] = "send"]
    /\ UNCHANGED <<pubSent, targets, chan, permit, drainPending, inflight, delivered, sent>>

(* drain_backlog: send the next held message *)
DrainSend(s) ==
    /\ phase[s] = "send"
    /\ held[s] /= <<>>
    /\ LET m == Head(held[s])
       IN  /\ held' = [held EXCEPT ![s] = Tail(@)]
           /\ inflight' = [inflight EXCEPT ![s] = @ \cup {m}]
           /\ sent' = [sent EXCEPT ![s] = Append(@, m)]
    /\ UNCHANGED <<pubSent, targets, chan, storage, phase, backlog, permit, drainPending, delivered>>

(* drain_backlog: batch done -- loop while the flag is armed *)
DrainFinish(s) ==
    /\ phase[s] = "send"
    /\ held[s] = <<>>
    /\ phase' = [phase EXCEPT ![s] = "idle"]
    /\ backlog' = [backlog EXCEPT ![s] = IF ClearFirst THEN @ ELSE FALSE]
    /\ drainPending' = [drainPending EXCEPT ![s] = backlog'[s]]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, permit, inflight, delivered, sent>>

(* Subscriber PUBACK arrives *)
Ack(s, m) ==
    /\ m \in inflight[s]
    /\ inflight' = [inflight EXCEPT ![s] = @ \ {m}]
    /\ delivered' = [delivered EXCEPT ![s] = @ \cup {m}]
    /\ drainPending' = [drainPending EXCEPT ![s] = @ \/ backlog[s]]
    /\ UNCHANGED <<pubSent, targets, chan, storage, held, phase, backlog, permit, sent>>

Next ==
    \/ \E p \in Pubs : PubStart(p)
    \/ \E p \in Pubs, s \in Subs : RouteBehind(p, s) \/ RouteEnqueue(p, s) \/ RouteTimeout(p, s)
    \/ \E s \in Subs :
        \/ Consume(s) \/ Notified(s)
        \/ DrainIdle(s) \/ DrainDefer(s) \/ DrainClear(s) \/ DrainTake(s) \/ DrainSend(s) \/ DrainFinish(s)
    \/ \E s \in Subs, m \in Msgs : Ack(s, m)

Fairness ==
    /\ \A p \in Pubs : WF_vars(PubStart(p))
    /\ \A p \in Pubs, s \in Subs : WF_vars(RouteBehind(p, s)) /\ WF_vars(RouteEnqueue(p, s))
    /\ \A s \in Subs :
        /\ WF_vars(Consume(s)) /\ WF_vars(Notified(s))
        /\ WF_vars(DrainIdle(s)) /\ WF_vars(DrainDefer(s)) /\ WF_vars(DrainClear(s))
        /\ WF_vars(DrainTake(s)) /\ WF_vars(DrainSend(s)) /\ WF_vars(DrainFinish(s))
    /\ \A s \in Subs, m \in Msgs : WF_vars(Ack(s, m))

Spec == Init /\ [][Next]_vars /\ Fairness

(* A message is routed to s once the router has finished with it for s *)
Routed(s) ==
    {m \in Msgs :
        /\ m.n <= pubSent[m.p]
        /\ ~(m.n = pubSent[m.p] /\ s \in targets[m.p])}

Custody(s) == Range(chan[s]) \cup Range(storage[s]) \cup Range(held[s]) \cup inflight[s] \cup delivered[s]

InvChanBound == \A s \in Subs : Len(chan[s]) <= Cap
InvWindowBound == \A s \in Subs : Cardinality(inflight[s]) <= RM
InvNoLoss == \A s \in Subs : Routed(s) = Custody(s)
InvNoDuplicate == \A s \in Subs :
    /\ Len(chan[s]) = Cardinality(Range(chan[s]))
    /\ Len(storage[s]) = Cardinality(Range(storage[s]))
    /\ Len(held[s]) = Cardinality(Range(held[s]))
    /\ Len(sent[s]) = Cardinality(Range(sent[s]))
    /\ Range(chan[s]) \cap Range(storage[s]) = {}
    /\ Range(chan[s]) \cap Range(held[s]) = {}
    /\ Range(chan[s]) \cap inflight[s] = {}
    /\ Range(storage[s]) \cap Range(held[s]) = {}
    /\ Range(storage[s]) \cap inflight[s] = {}
    /\ Range(held[s]) \cap inflight[s] = {}
    /\ delivered[s] \cap (Range(chan[s]) \cup Range(storage[s]) \cup Range(held[s]) \cup inflight[s]) = {}
InvBacklogArmed == \A s \in Subs :
    (storage[s] /= <<>> /\ phase[s] = "idle") => (backlog[s] \/ permit[s] \/ drainPending[s])
InvFifo == \A s \in Subs : \A i, j \in DOMAIN sent[s] :
    (i < j /\ sent[s][i].p = sent[s][j].p) => sent[s][i].n < sent[s][j].n

AllDelivered == \A s \in Subs : delivered[s] = Msgs
Live == <>AllDelivered

=============================================================================
