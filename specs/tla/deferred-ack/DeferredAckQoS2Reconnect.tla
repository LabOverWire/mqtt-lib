--------------------- MODULE DeferredAckQoS2Reconnect ---------------------
(***************************************************************************)
(* Stage 5 (faithful, v3): inbound QoS2 with a DEFERRED PUBREC, where         *)
(* duplicate PUBLISHes arise the ONLY way MQTT-5 permits -- from a session      *)
(* resume on RECONNECT, never from in-connection retransmission                 *)
(* ([MQTT-4.4.0-1]).                                                            *)
(*                                                                          *)
(* v3 CHANGES (post-implementation spec review): the model is corrected to      *)
(* match MQTT-5 and the shipped code on the REJECT and packet-id-REUSE paths.    *)
(*  - [MQTT-4.3.3-9]: after a receiver sends a PUBREC with Reason Code >= 0x80    *)
(*    (a reject), it MUST treat any subsequent PUBLISH with that Packet          *)
(*    Identifier as a NEW Application Message. So `AppReject` now CLEARS the      *)
(*    per-id dedup state (delivered) instead of recording a "reject" resolution;  *)
(*    a later same-id PUBLISH re-delivers. The old "keep reject + re-send error   *)
(*    PUBREC + never re-deliver" was an artifact that over-claimed exactly-once   *)
(*    for rejected messages, a guarantee the spec does not make.                 *)
(*  - PACKET-ID REUSE: after an exchange completes (`ClientRecvPubrel` clears     *)
(*    delivered+resolution, mirroring handle_pubrel->clear_inbound_state) or is    *)
(*    rejected, the broker may reuse the id for a NEW message. `ReArm` models a    *)
(*    `done` id being re-issued as a fresh instance; `deliveryCount` and           *)
(*    `wasRejected` reset per instance.                                            *)
(*  - `InvNoDoubleDelivery` is now scoped: exactly-once DELIVERY holds for         *)
(*    messages that were NOT rejected (`~wasRejected`). Rejected messages are      *)
(*    at-least-once (a lost error PUBREC + reconnect replay re-delivers), which is  *)
(*    spec-compliant and what the app-facing docs promise.                         *)
(*                                                                          *)
(* Persistence regimes (unchanged from v2): the client's SessionState dedup bits  *)
(* survive a transport reconnect iff PersistDelivered; the app token iff           *)
(* PersistToken. Transport reconnect = both TRUE (exactly-once for non-rejected);  *)
(* process crash = both FALSE (at-least-once processing); the forbidden middle     *)
(* (delivered persisted, token lost) is the InvNoWedge counterexample.            *)
(*                                                                          *)
(* Negative controls: BuggyDupPubrec=TRUE emits a PUBREC before the app resolves   *)
(* (InvDeferralHeld / wire violated); PersistDelivered=TRUE, PersistToken=FALSE     *)
(* wedges (InvNoWedge violated). InvNoDoubleDelivery is discriminating: it is       *)
(* checked in _transport and would FAIL under _crash (re-delivery), which is why    *)
(* _crash omits it. The _window config (MaxMsgs=3 > ReceiveMax=2) makes             *)
(* InvWindowBound non-vacuous -- at MaxMsgs=ReceiveMax the backpressure guard never  *)
(* binds, so the window property is only genuinely tested there.                     *)
(*                                                                          *)
(* SCOPE LIMITS (what this model does NOT prove -- verified for the checked          *)
(* constants only, and safety only):                                                 *)
(*  - session_present is assumed TRUE on every Reconnect: the broker always keeps    *)
(*    serverState. The model does NOT cover session_present=FALSE (broker restart /   *)
(*    session expiry) while the client still holds a persisted `delivered` bit --      *)
(*    that path can silently SUPPRESS a genuinely new same-id PUBLISH (message loss),  *)
(*    and no invariant here catches it. Tracked as follow-up #1 (session_present=0).   *)
(*  - No liveness: only safety invariants are checked. "Every accepted message is      *)
(*    eventually delivered / every held token eventually resolves" is NOT asserted;    *)
(*    InvNoWedge is a safety proxy for one specific stuck configuration only.           *)
(*  - InvNoDoubleDelivery's ~wasRejected exclusion is per-instance sticky: a message    *)
(*    rejected once then accepted on replay (deliveryCount=2, wasRejected=TRUE) is       *)
(*    excluded. That is spec-honest at-least-once for a rejected id, but it means the    *)
(*    invariant does not bound delivery of an ACCEPTED payload that had a prior reject   *)
(*    on the same id in the same instance.                                              *)
(*  - ReArm abstracts the broker reusing a packet id only after BOTH wire directions    *)
(*    are drained of that id. The live-send allocator (next_packet_id) does not itself  *)
(*    enforce this on u16 wraparound; see follow-up on next_packet_id id-reuse safety.  *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    MaxMsgs,          \* distinct packet ids modelled
    ReceiveMax,       \* inbound Receive-Maximum window
    MaxReconnects,    \* bound on disconnect/reconnect cycles
    MaxReuse,         \* bound on how many times a packet id is re-issued (ReArm)
    PersistDelivered, \* client SessionState dedup bits survive reconnect
    PersistToken,     \* app in-memory AckToken survives reconnect
    BuggyDupPubrec    \* TRUE = DUP path emits PUBREC for an unresolved id (bug)

Msgs == 1..MaxMsgs
S2CPacket == [type: {"pub", "pubrel"}, id: Msgs]
PubrecPacket  == [type: {"pubrec"},  id: Msgs, ok: BOOLEAN]
PubcompPacket == [type: {"pubcomp"}, id: Msgs]
C2SPacket == PubrecPacket \cup PubcompPacket

VARIABLES
    connected,      \* is the network connection up
    cycles,         \* disconnect/reconnect cycles used (bounds the model)
    serverState,    \* per id: toSend -> awaitPubrec -> awaitPubcomp -> done
    pendingResend,  \* per id: set on reconnect, drained by a single replay send
    s2c,            \* ORDERED server -> client packets THIS connection
    c2s,            \* client -> server acks in flight THIS connection
    delivered,      \* client dedup guard: id handed to the app at least once
    resolution,     \* client: in-flight decision, none | ack (reject CLEARS to none)
    pubrecSent,     \* client has_pubrec: success PUBREC written, awaiting PUBREL
    hasToken,       \* app currently holds the AckToken (the resolve capability)
    ackedEver,      \* durable fact: the app ACKED this instance (reset on ReArm)
    deliveryCount,  \* per instance: times the Application Message reached the app
    wasRejected,    \* the app REJECTED this instance (reset on ReArm)
    instanceCount   \* how many times this id has been re-issued (bounds ReArm)

vars == <<connected, cycles, serverState, pendingResend, s2c, c2s, delivered,
          resolution, pubrecSent, hasToken, ackedEver, deliveryCount,
          wasRejected, instanceCount>>

Outstanding == {m \in Msgs : serverState[m] \in {"awaitPubrec", "awaitPubcomp"}}

(* The app has made a terminal decision (ack or reject) on this instance. Used by  *)
(* the wire invariant: reject clears `resolution`, so `resolution # none` alone     *)
(* would wrongly flag the legitimate error PUBREC a reject puts on the wire.        *)
EverActed(m) == ackedEver[m] \/ wasRejected[m]

Init ==
    /\ connected = TRUE
    /\ cycles = 0
    /\ serverState = [m \in Msgs |-> "toSend"]
    /\ pendingResend = [m \in Msgs |-> FALSE]
    /\ s2c = <<>>
    /\ c2s = {}
    /\ delivered = [m \in Msgs |-> FALSE]
    /\ resolution = [m \in Msgs |-> "none"]
    /\ pubrecSent = [m \in Msgs |-> FALSE]
    /\ hasToken = [m \in Msgs |-> FALSE]
    /\ ackedEver = [m \in Msgs |-> FALSE]
    /\ deliveryCount = [m \in Msgs |-> 0]
    /\ wasRejected = [m \in Msgs |-> FALSE]
    /\ instanceCount = [m \in Msgs |-> 0]

----------------------------------------------------------------------------
(* Server (broker as sender to the subscribing client). *)

ServerSendNew(m) ==
    /\ connected
    /\ serverState[m] = "toSend"
    /\ Cardinality(Outstanding) < ReceiveMax
    /\ s2c' = Append(s2c, [type |-> "pub", id |-> m])
    /\ serverState' = [serverState EXCEPT ![m] = "awaitPubrec"]
    /\ UNCHANGED <<connected, cycles, pendingResend, c2s, delivered, resolution,
                   pubrecSent, hasToken, ackedEver, deliveryCount, wasRejected,
                   instanceCount>>

(* Post-reconnect replay of an unacked PUBLISH (dup=true). Enabled ONLY because *)
(* Reconnect armed pendingResend -- never spontaneously on a live link.         *)
ServerResendPub(m) ==
    /\ connected
    /\ pendingResend[m]
    /\ serverState[m] = "awaitPubrec"
    /\ s2c' = Append(s2c, [type |-> "pub", id |-> m])
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, serverState, c2s, delivered, resolution,
                   pubrecSent, hasToken, ackedEver, deliveryCount, wasRejected,
                   instanceCount>>

ServerResendPubrel(m) ==
    /\ connected
    /\ pendingResend[m]
    /\ serverState[m] = "awaitPubcomp"
    /\ s2c' = Append(s2c, [type |-> "pubrel", id |-> m])
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, serverState, c2s, delivered, resolution,
                   pubrecSent, hasToken, ackedEver, deliveryCount, wasRejected,
                   instanceCount>>

(* A phase transition clears any armed replay latch: the id has moved on, so a    *)
(* stale pendingResend must not fire a live-connection retransmit.                *)
ServerRecvPubrecOk(m) ==
    /\ connected
    /\ [type |-> "pubrec", id |-> m, ok |-> TRUE] \in c2s
    /\ serverState[m] = "awaitPubrec"
    /\ c2s' = c2s \ {[type |-> "pubrec", id |-> m, ok |-> TRUE]}
    /\ serverState' = [serverState EXCEPT ![m] = "awaitPubcomp"]
    /\ s2c' = Append(s2c, [type |-> "pubrel", id |-> m])
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, delivered, resolution, pubrecSent,
                   hasToken, ackedEver, deliveryCount, wasRejected, instanceCount>>

(* [MQTT-4.3.3-9] / [MQTT-4.3.3-4]: an error PUBREC (ok=FALSE) terminates the      *)
(* exchange -- the sender discards the message, sends NO PUBREL, and releases the   *)
(* id (serverState -> done, reusable via ReArm).                                    *)
ServerRecvPubrecErr(m) ==
    /\ connected
    /\ [type |-> "pubrec", id |-> m, ok |-> FALSE] \in c2s
    /\ serverState[m] = "awaitPubrec"
    /\ c2s' = c2s \ {[type |-> "pubrec", id |-> m, ok |-> FALSE]}
    /\ serverState' = [serverState EXCEPT ![m] = "done"]
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, s2c, delivered, resolution, pubrecSent,
                   hasToken, ackedEver, deliveryCount, wasRejected, instanceCount>>

ServerRecvPubcomp(m) ==
    /\ connected
    /\ [type |-> "pubcomp", id |-> m] \in c2s
    /\ c2s' = c2s \ {[type |-> "pubcomp", id |-> m]}
    /\ serverState' = [serverState EXCEPT ![m] = "done"]
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, s2c, delivered, resolution, pubrecSent,
                   hasToken, ackedEver, deliveryCount, wasRejected, instanceCount>>

(* The broker re-issues a released packet id for a genuinely NEW message. A `done`  *)
(* id returns to `toSend`, and the per-INSTANCE model bookkeeping resets. It does    *)
(* NOT touch the client's protocol state (delivered/resolution/pubrecSent/hasToken)  *)
(* -- a broker cannot reset a client -- and is gated on the client being drained, so *)
(* a reused id can never inherit stale client state. The drain happens via the        *)
(* client's own actions (ClientRecvPubrel) plus the broker self-heal below.          *)
ReArm(m) ==
    /\ connected
    /\ serverState[m] = "done"
    /\ instanceCount[m] < MaxReuse
    /\ ~delivered[m] /\ ~pubrecSent[m] /\ ~hasToken[m] /\ resolution[m] = "none"
    /\ ~(\E p \in c2s : p.id = m)              \* no in-flight ack (client->server) for this id
    /\ ~(\E i \in 1..Len(s2c) : s2c[i].id = m) \* no in-flight packet (server->client) for this id
    /\ serverState' = [serverState EXCEPT ![m] = "toSend"]
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ deliveryCount' = [deliveryCount EXCEPT ![m] = 0]
    /\ ackedEver' = [ackedEver EXCEPT ![m] = FALSE]
    /\ wasRejected' = [wasRejected EXCEPT ![m] = FALSE]
    /\ instanceCount' = [instanceCount EXCEPT ![m] = @ + 1]
    /\ UNCHANGED <<connected, cycles, s2c, c2s, delivered, resolution,
                   pubrecSent, hasToken>>

(* Broker self-heal. A success PUBREC arrives for an id the broker already released  *)
(* (done) -- this happens when the app rejects a message but then acks a re-delivery  *)
(* of the SAME buffered PUBLISH after a reconnect. The real broker's PUBREC handler     *)
(* writes a PUBREL with reason Success unconditionally; a missing inflight only skips    *)
(* the storage update, not the PUBREL, so a released id still receives a PUBREL. The      *)
(* client's ClientRecvPubrel then clears its stale dedup state, so the id is drained      *)
(* before it can be reused.                                                              *)
ServerRecvPubrecReleased(m) ==
    /\ connected
    /\ [type |-> "pubrec", id |-> m, ok |-> TRUE] \in c2s
    /\ serverState[m] = "done"
    /\ c2s' = c2s \ {[type |-> "pubrec", id |-> m, ok |-> TRUE]}
    /\ s2c' = Append(s2c, [type |-> "pubrel", id |-> m])
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, delivered,
                   resolution, pubrecSent, hasToken, ackedEver, deliveryCount,
                   wasRejected, instanceCount>>

(* A late ERROR PUBREC for an already-released id (the app rejected a re-delivery    *)
(* too) is silently discarded: the broker has no inflight for the id, so its error    *)
(* branch removes nothing and sends nothing. Draining it clears the wire so the id    *)
(* can be reused.                                                                     *)
ServerDiscardStaleErrPubrec(m) ==
    /\ connected
    /\ [type |-> "pubrec", id |-> m, ok |-> FALSE] \in c2s
    /\ serverState[m] = "done"
    /\ c2s' = c2s \ {[type |-> "pubrec", id |-> m, ok |-> FALSE]}
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, s2c, delivered,
                   resolution, pubrecSent, hasToken, ackedEver, deliveryCount,
                   wasRejected, instanceCount>>

----------------------------------------------------------------------------
(* Client (receiver). *)

(* First receipt: DELIVER, arm the dedup guard, and HAND the app a token. *)
ClientRecvPubFirst ==
    /\ connected
    /\ s2c # <<>>
    /\ LET h == Head(s2c) IN
         /\ h.type = "pub"
         /\ ~delivered[h.id]
         /\ delivered' = [delivered EXCEPT ![h.id] = TRUE]
         /\ hasToken' = [hasToken EXCEPT ![h.id] = TRUE]
         /\ deliveryCount' = [deliveryCount EXCEPT ![h.id] =
                                IF @ < 2 THEN @ + 1 ELSE @]
    /\ s2c' = Tail(s2c)
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, c2s,
                   resolution, pubrecSent, ackedEver, wasRejected, instanceCount>>

(* A DUP for an already-delivered id: SUPPRESS. Re-send the success PUBREC if the  *)
(* app acked (guard still held, awaiting PUBREL); send NOTHING if unresolved. A     *)
(* rejected or completed id has its guard CLEARED, so a replay is a first receipt   *)
(* (ClientRecvPubFirst), never seen here. BuggyDupPubrec emits a PUBREC for an       *)
(* unresolved id -- the deferral-integrity violation.                               *)
ClientRecvPubDup ==
    /\ connected
    /\ s2c # <<>>
    /\ LET h == Head(s2c) IN
         /\ h.type = "pub"
         /\ delivered[h.id]
         /\ IF pubrecSent[h.id]
              THEN /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> h.id, ok |-> TRUE]}
                   /\ UNCHANGED pubrecSent
              ELSE IF BuggyDupPubrec
                THEN /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> h.id, ok |-> TRUE]}
                     /\ pubrecSent' = [pubrecSent EXCEPT ![h.id] = TRUE]
                ELSE UNCHANGED <<c2s, pubrecSent>>
    /\ s2c' = Tail(s2c)
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, delivered,
                   resolution, hasToken, ackedEver, deliveryCount, wasRejected,
                   instanceCount>>

(* The app resolves the token (deferred ack). Requires holding the token. *)
AppAck(m) ==
    /\ connected
    /\ hasToken[m]
    /\ resolution[m] = "none"
    /\ resolution' = [resolution EXCEPT ![m] = "ack"]
    /\ pubrecSent' = [pubrecSent EXCEPT ![m] = TRUE]
    /\ ackedEver' = [ackedEver EXCEPT ![m] = TRUE]
    /\ hasToken' = [hasToken EXCEPT ![m] = FALSE]
    /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> m, ok |-> TRUE]}
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, s2c,
                   delivered, deliveryCount, wasRejected, instanceCount>>

(* reject / Drop-with-reason: emit the error PUBREC and CLEAR the per-id dedup      *)
(* state ([MQTT-4.3.3-9] -- a later same-id PUBLISH is a new message). `wasRejected` *)
(* records the decision for the wire invariant and the scoped exactly-once bound.    *)
AppReject(m) ==
    /\ connected
    /\ hasToken[m]
    /\ resolution[m] = "none"
    /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> m, ok |-> FALSE]}
    /\ delivered' = [delivered EXCEPT ![m] = FALSE]
    /\ hasToken' = [hasToken EXCEPT ![m] = FALSE]
    /\ wasRejected' = [wasRejected EXCEPT ![m] = TRUE]
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, s2c,
                   resolution, pubrecSent, ackedEver, deliveryCount, instanceCount>>

(* handle_pubrel -> clear_inbound_state: send PUBCOMP, clear has_pubrec AND the     *)
(* dedup guard + resolution, so the completed id can be reused for a new message.   *)
ClientRecvPubrel ==
    /\ connected
    /\ s2c # <<>>
    /\ LET h == Head(s2c) IN
         /\ h.type = "pubrel"
         /\ pubrecSent' = [pubrecSent EXCEPT ![h.id] = FALSE]
         /\ delivered' = [delivered EXCEPT ![h.id] = FALSE]
         /\ resolution' = [resolution EXCEPT ![h.id] = "none"]
         /\ c2s' = c2s \cup {[type |-> "pubcomp", id |-> h.id]}
    /\ s2c' = Tail(s2c)
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, hasToken,
                   ackedEver, deliveryCount, wasRejected, instanceCount>>

----------------------------------------------------------------------------
(* Connection lifecycle. *)

Disconnect ==
    /\ connected
    /\ cycles < MaxReconnects
    /\ connected' = FALSE
    /\ cycles' = cycles + 1
    /\ s2c' = <<>>
    /\ c2s' = {}
    /\ pendingResend' = [m \in Msgs |-> FALSE]
    /\ UNCHANGED <<serverState, delivered, resolution, pubrecSent, hasToken,
                   ackedEver, deliveryCount, wasRejected, instanceCount>>

(* Reconnect with the session present. The broker arms a replay of every still-  *)
(* outstanding PUBLISH/PUBREL. Client dedup bits (delivered/resolution/pubrecSent/ *)
(* wasRejected) survive iff PersistDelivered; the app token iff PersistToken.       *)
Reconnect ==
    /\ ~connected
    /\ connected' = TRUE
    /\ pendingResend' = [m \in Msgs |-> serverState[m] \in {"awaitPubrec", "awaitPubcomp"}]
    /\ IF PersistDelivered
         THEN UNCHANGED <<delivered, resolution, pubrecSent, wasRejected>>
         ELSE /\ delivered' = [m \in Msgs |-> FALSE]
              /\ resolution' = [m \in Msgs |-> "none"]
              /\ pubrecSent' = [m \in Msgs |-> FALSE]
              /\ wasRejected' = [m \in Msgs |-> FALSE]
    /\ IF PersistToken
         THEN UNCHANGED hasToken
         ELSE hasToken' = [m \in Msgs |-> FALSE]
    /\ UNCHANGED <<cycles, serverState, s2c, c2s, ackedEver, deliveryCount,
                   instanceCount>>

AllDone == \A m \in Msgs : serverState[m] = "done" /\ instanceCount[m] = MaxReuse
Done == AllDone /\ UNCHANGED vars

Next ==
    \/ \E m \in Msgs : ServerSendNew(m)
    \/ \E m \in Msgs : ServerResendPub(m)
    \/ \E m \in Msgs : ServerResendPubrel(m)
    \/ \E m \in Msgs : ServerRecvPubrecOk(m)
    \/ \E m \in Msgs : ServerRecvPubrecErr(m)
    \/ \E m \in Msgs : ServerRecvPubcomp(m)
    \/ \E m \in Msgs : ServerRecvPubrecReleased(m)
    \/ \E m \in Msgs : ServerDiscardStaleErrPubrec(m)
    \/ \E m \in Msgs : ReArm(m)
    \/ ClientRecvPubFirst
    \/ ClientRecvPubDup
    \/ \E m \in Msgs : AppAck(m)
    \/ \E m \in Msgs : AppReject(m)
    \/ ClientRecvPubrel
    \/ Disconnect
    \/ Reconnect
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ connected \in BOOLEAN
    /\ cycles \in 0..MaxReconnects
    /\ serverState \in [Msgs -> {"toSend", "awaitPubrec", "awaitPubcomp", "done"}]
    /\ pendingResend \in [Msgs -> BOOLEAN]
    /\ \A i \in 1..Len(s2c) : s2c[i] \in S2CPacket
    /\ c2s \subseteq C2SPacket
    /\ delivered \in [Msgs -> BOOLEAN]
    /\ resolution \in [Msgs -> {"none", "ack"}]
    /\ pubrecSent \in [Msgs -> BOOLEAN]
    /\ hasToken \in [Msgs -> BOOLEAN]
    /\ ackedEver \in [Msgs -> BOOLEAN]
    /\ deliveryCount \in [Msgs -> 0..2]
    /\ wasRejected \in [Msgs -> BOOLEAN]
    /\ instanceCount \in [Msgs -> 0..MaxReuse]

Resolved(m) == resolution[m] # "none"

(* Exactly-once DELIVERY for NON-rejected messages (per instance). Holds in the    *)
(* transport-reconnect regime; deliberately NOT asserted under a process crash      *)
(* (at-least-once processing). Rejected messages are excluded: [MQTT-4.3.3-9]       *)
(* makes them at-least-once (a lost error PUBREC + reconnect replay re-delivers).   *)
InvNoDoubleDelivery ==
    \A m \in Msgs : ~wasRejected[m] => deliveryCount[m] <= 1

(* NO PERMANENT WEDGE: a delivered id the app can no longer resolve (no token,      *)
(* unresolved) while the broker still awaits its PUBREC is stuck. Reachable ONLY    *)
(* when `delivered` is persisted but the token is not.                              *)
InvNoWedge ==
    \A m \in Msgs :
        ~(delivered[m] /\ ~hasToken[m] /\ resolution[m] = "none"
          /\ ~wasRejected[m] /\ serverState[m] = "awaitPubrec")

(* DEFERRAL INTEGRITY. State bit: has_pubrec only after a genuine ack. Wire: no     *)
(* PUBREC of any kind (success from ack, error from reject) reaches the wire before  *)
(* the app has ACTED. `EverActed` covers reject, whose PUBREC is legitimate even     *)
(* though it clears `resolution`.                                                    *)
InvDeferralHeld == \A m \in Msgs : pubrecSent[m] => Resolved(m)
InvNoPubrecOnWireBeforeResolve ==
    \A p \in c2s : p.type = "pubrec" => EverActed(p.id)

(* TRUE at-least-once PROCESSING: the handshake advances past awaitPubrec only     *)
(* because the app genuinely ACKED, never via a fabrication.                        *)
InvPubrelAfterResolve ==
    \A m \in Msgs : serverState[m] = "awaitPubcomp" => ackedEver[m]

(* Backpressure: outstanding QoS2 ids never exceed the Receive-Maximum window. *)
InvWindowBound == Cardinality(Outstanding) <= ReceiveMax

(* No ack without a delivery (for the in-flight ack decision; reject clears both). *)
InvResolvedImpliesDelivered == \A m \in Msgs : Resolved(m) => delivered[m]

(* No dangling has_pubrec after a NORMAL completion: a `done` id that was not         *)
(* rejected owes no PUBREL (ClientRecvPubrel clears pubrecSent before the PUBCOMP      *)
(* that drives the server to `done`). The reject case is excluded: after a reject the  *)
(* app may ack a re-delivery of the same buffered PUBLISH, legitimately setting         *)
(* pubrecSent for a NEW logical message while the server is `done` on the OLD one --    *)
(* a benign transient the broker self-heal (ServerRecvPubrecReleased) then drains.     *)
InvDoneImpliesNoPubrec ==
    \A m \in Msgs : (serverState[m] = "done" /\ ~wasRejected[m]) => ~pubrecSent[m]

(* CLEAN RE-ARM: a fresh or re-issued id (serverState = "toSend": the initial state or  *)
(* a ReArm'd id) carries NO stale client dedup state. This is the independent guard on   *)
(* packet-id reuse. It would catch the reuse-resurrection bug class -- a completed or     *)
(* rejected id whose dedup guard was left set, so a later reused id SUPPRESSES its new    *)
(* message (silent under-delivery). InvNoDoubleDelivery only sees over-delivery; this     *)
(* is the complementary under-delivery guard, and it is exactly the property the two      *)
(* review-117 code bugs (dedup guard not cleared on completion / on reject) violated.     *)
InvFreshIdNoStaleState ==
    \A m \in Msgs :
        serverState[m] = "toSend" =>
            (~delivered[m] /\ ~pubrecSent[m] /\ resolution[m] = "none")
============================================================================
