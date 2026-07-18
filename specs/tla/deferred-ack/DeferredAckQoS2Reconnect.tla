--------------------- MODULE DeferredAckQoS2Reconnect ---------------------
(***************************************************************************)
(* Stage 5 (faithful, v2): inbound QoS2 with a DEFERRED PUBREC, where          *)
(* duplicate PUBLISHes arise the ONLY way MQTT-5 permits -- from a session      *)
(* resume on RECONNECT, never from in-connection retransmission                 *)
(* ([MQTT-4.4.0-1]; the broker resends only in resend_inflight_messages,        *)
(* broker/client_handler/publish.rs, gated by session_present in connect.rs).   *)
(*                                                                          *)
(* v2 CHANGES (from the confirmation quorum):                                 *)
(*  - The client's post-crash state is modelled with TWO independent           *)
(*    persistence axes, because a real crash loses different things:            *)
(*      PersistDelivered -- the SessionState dedup bits (delivered / resolution *)
(*                          / pubrecSent). Survive a TRANSPORT reconnect (kept   *)
(*                          in memory); lost on a process crash UNLESS persisted *)
(*                          to disk. (inbound_pubrecs is in-memory only today.)  *)
(*      PersistToken     -- the app's in-memory AckToken (`hasToken`). Survives  *)
(*                          a transport reconnect; ALWAYS lost on a process      *)
(*                          crash (it is a live object, never serialised).       *)
(*  - The three real scenarios:                                                 *)
(*      TRANSPORT reconnect  (Persist* = TRUE, TRUE): session + token survive.   *)
(*          => exactly-once DELIVERY (InvNoDoubleDelivery holds).                *)
(*      PROCESS crash        (Persist* = FALSE, FALSE): all client state gone.   *)
(*          => the replay re-delivers -> at-least-once PROCESSING (the           *)
(*             documented contract, decision 5.2 / Stage 4a). Double DELIVERY is *)
(*             EXPECTED here, so InvNoDoubleDelivery is deliberately NOT asserted *)
(*             in the crash config. A fresh token is minted on re-delivery, so   *)
(*             there is NO wedge (InvNoWedge holds).                             *)
(*      PERSIST MISTAKE      (PersistDelivered=TRUE, PersistToken=FALSE): the    *)
(*          dedup bits were persisted to disk but the token was lost on crash.   *)
(*          => the replay is SUPPRESSED (delivered=TRUE) yet NO token is minted, *)
(*             so the app can never resolve and the broker's slot wedges         *)
(*             forever. This is the negative control for InvNoWedge, and the     *)
(*             formal reason a design MUST NOT persist `delivered` without also   *)
(*             re-minting the resolve capability on re-delivery.                 *)
(*  - F2 fix: pendingResend[m] is cleared on every server phase transition, so   *)
(*    the reconnect replay latch can never fire a spurious PUBREL on a live      *)
(*    connection.                                                               *)
(*                                                                          *)
(* Client keeps: delivered (dedup guard), resolution in {none,ack,reject},      *)
(* pubrecSent (has_pubrec: success PUBREC written, awaiting PUBREL; set at ack,  *)
(* cleared at PUBREL, NOT set at reject), hasToken (app holds the resolve        *)
(* capability), and a durable monotone ackedEver (the app acked at some point).  *)
(* Receive-Maximum: the broker holds a slot per outstanding id                  *)
(* (awaitPubrec/awaitPubcomp); a held (unresolved) token keeps its id            *)
(* awaitPubrec, throttling new sends -- that IS the backpressure.               *)
(*                                                                          *)
(* Other negative control: BuggyDupPubrec=TRUE -> a DUP for an unresolved id     *)
(* emits PUBREC -> InvDeferralHeld / InvNoPubrecOnWireBeforeResolve VIOLATED.    *)
(*                                                                          *)
(* Scope: exactly-once DELIVERY (transport), at-least-once PROCESSING + no-wedge *)
(* (crash), deferral integrity, window bound, reject terminality. The Drop/leak  *)
(* wedge on a live connection is Stage 4b; clean-vs-persistent loss is Stage 4a. *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    MaxMsgs,          \* distinct packet ids modelled
    ReceiveMax,       \* inbound Receive-Maximum window
    MaxReconnects,    \* bound on disconnect/reconnect cycles
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
    resolution,     \* client: app's terminal decision, none | ack | reject
    pubrecSent,     \* client has_pubrec: success PUBREC written, awaiting PUBREL
    hasToken,       \* app currently holds the AckToken (the resolve capability)
    ackedEver,      \* durable fact: the app ACKED this id at some point (never cleared)
    deliveryCount   \* per id: times the Application Message reached the app

vars == <<connected, cycles, serverState, pendingResend, s2c, c2s, delivered,
          resolution, pubrecSent, hasToken, ackedEver, deliveryCount>>

Outstanding == {m \in Msgs : serverState[m] \in {"awaitPubrec", "awaitPubcomp"}}

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

----------------------------------------------------------------------------
(* Server (broker as sender to the subscribing client). *)

ServerSendNew(m) ==
    /\ connected
    /\ serverState[m] = "toSend"
    /\ Cardinality(Outstanding) < ReceiveMax
    /\ s2c' = Append(s2c, [type |-> "pub", id |-> m])
    /\ serverState' = [serverState EXCEPT ![m] = "awaitPubrec"]
    /\ UNCHANGED <<connected, cycles, pendingResend, c2s, delivered, resolution,
                   pubrecSent, hasToken, ackedEver, deliveryCount>>

(* Post-reconnect replay of an unacked PUBLISH (dup=true). Enabled ONLY because *)
(* Reconnect armed pendingResend -- never spontaneously on a live link.         *)
ServerResendPub(m) ==
    /\ connected
    /\ pendingResend[m]
    /\ serverState[m] = "awaitPubrec"
    /\ s2c' = Append(s2c, [type |-> "pub", id |-> m])
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, serverState, c2s, delivered, resolution,
                   pubrecSent, hasToken, ackedEver, deliveryCount>>

ServerResendPubrel(m) ==
    /\ connected
    /\ pendingResend[m]
    /\ serverState[m] = "awaitPubcomp"
    /\ s2c' = Append(s2c, [type |-> "pubrel", id |-> m])
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, serverState, c2s, delivered, resolution,
                   pubrecSent, hasToken, ackedEver, deliveryCount>>

(* A phase transition clears any armed replay latch (F2 fix): the id has moved   *)
(* on, so a stale pendingResend must not fire a live-connection retransmit.      *)
ServerRecvPubrecOk(m) ==
    /\ connected
    /\ [type |-> "pubrec", id |-> m, ok |-> TRUE] \in c2s
    /\ serverState[m] = "awaitPubrec"
    /\ c2s' = c2s \ {[type |-> "pubrec", id |-> m, ok |-> TRUE]}
    /\ serverState' = [serverState EXCEPT ![m] = "awaitPubcomp"]
    /\ s2c' = Append(s2c, [type |-> "pubrel", id |-> m])
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, delivered, resolution, pubrecSent,
                   hasToken, ackedEver, deliveryCount>>

ServerRecvPubrecErr(m) ==
    /\ connected
    /\ [type |-> "pubrec", id |-> m, ok |-> FALSE] \in c2s
    /\ serverState[m] = "awaitPubrec"
    /\ c2s' = c2s \ {[type |-> "pubrec", id |-> m, ok |-> FALSE]}
    /\ serverState' = [serverState EXCEPT ![m] = "done"]
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, s2c, delivered, resolution, pubrecSent,
                   hasToken, ackedEver, deliveryCount>>

ServerRecvPubcomp(m) ==
    /\ connected
    /\ [type |-> "pubcomp", id |-> m] \in c2s
    /\ c2s' = c2s \ {[type |-> "pubcomp", id |-> m]}
    /\ serverState' = [serverState EXCEPT ![m] = "done"]
    /\ pendingResend' = [pendingResend EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<connected, cycles, s2c, delivered, resolution, pubrecSent,
                   hasToken, ackedEver, deliveryCount>>

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
                   resolution, pubrecSent, ackedEver>>

(* A DUP for an already-delivered id: SUPPRESS. Re-send the ack that matches the *)
(* app's decision -- success if acked, error if rejected, NOTHING if unresolved. *)
(* NOTE: if delivered=TRUE but the token was lost (persist-mistake) and the id is *)
(* unresolved, this sends nothing and no token is re-minted => the wedge.         *)
ClientRecvPubDup ==
    /\ connected
    /\ s2c # <<>>
    /\ LET h == Head(s2c) IN
         /\ h.type = "pub"
         /\ delivered[h.id]
         /\ IF pubrecSent[h.id]
              THEN /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> h.id, ok |-> TRUE]}
                   /\ UNCHANGED <<resolution, pubrecSent>>
              ELSE IF resolution[h.id] = "reject"
                THEN /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> h.id, ok |-> FALSE]}
                     /\ UNCHANGED <<resolution, pubrecSent>>
              ELSE IF resolution[h.id] = "none"
                THEN IF BuggyDupPubrec
                       THEN /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> h.id, ok |-> TRUE]}
                            /\ pubrecSent' = [pubrecSent EXCEPT ![h.id] = TRUE]
                            /\ UNCHANGED resolution
                       ELSE UNCHANGED <<c2s, resolution, pubrecSent>>
              ELSE UNCHANGED <<c2s, resolution, pubrecSent>>
    /\ s2c' = Tail(s2c)
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, delivered,
                   hasToken, ackedEver, deliveryCount>>

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
                   delivered, deliveryCount>>

(* reject / Drop-with-reason (decision 5.1): terminal error PUBREC, no has_pubrec.*)
AppReject(m) ==
    /\ connected
    /\ hasToken[m]
    /\ resolution[m] = "none"
    /\ resolution' = [resolution EXCEPT ![m] = "reject"]
    /\ hasToken' = [hasToken EXCEPT ![m] = FALSE]
    /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> m, ok |-> FALSE]}
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, s2c,
                   delivered, pubrecSent, ackedEver, deliveryCount>>

(* handle_pubrel: send PUBCOMP unconditionally, clear has_pubrec. *)
ClientRecvPubrel ==
    /\ connected
    /\ s2c # <<>>
    /\ LET h == Head(s2c) IN
         /\ h.type = "pubrel"
         /\ pubrecSent' = [pubrecSent EXCEPT ![h.id] = FALSE]
         /\ c2s' = c2s \cup {[type |-> "pubcomp", id |-> h.id]}
    /\ s2c' = Tail(s2c)
    /\ UNCHANGED <<connected, cycles, serverState, pendingResend, delivered,
                   resolution, hasToken, ackedEver, deliveryCount>>

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
                   ackedEver, deliveryCount>>

(* Reconnect with the session present. The broker arms a replay of every still-  *)
(* outstanding PUBLISH/PUBREL. Client dedup bits survive iff PersistDelivered;    *)
(* the app token survives iff PersistToken. ackedEver is durable (always kept).   *)
Reconnect ==
    /\ ~connected
    /\ connected' = TRUE
    /\ pendingResend' = [m \in Msgs |-> serverState[m] \in {"awaitPubrec", "awaitPubcomp"}]
    /\ IF PersistDelivered
         THEN UNCHANGED <<delivered, resolution, pubrecSent>>
         ELSE /\ delivered' = [m \in Msgs |-> FALSE]
              /\ resolution' = [m \in Msgs |-> "none"]
              /\ pubrecSent' = [m \in Msgs |-> FALSE]
    /\ IF PersistToken
         THEN UNCHANGED hasToken
         ELSE hasToken' = [m \in Msgs |-> FALSE]
    /\ UNCHANGED <<cycles, serverState, s2c, c2s, ackedEver, deliveryCount>>

AllDone == \A m \in Msgs : serverState[m] = "done"
Done == AllDone /\ UNCHANGED vars

Next ==
    \/ \E m \in Msgs : ServerSendNew(m)
    \/ \E m \in Msgs : ServerResendPub(m)
    \/ \E m \in Msgs : ServerResendPubrel(m)
    \/ \E m \in Msgs : ServerRecvPubrecOk(m)
    \/ \E m \in Msgs : ServerRecvPubrecErr(m)
    \/ \E m \in Msgs : ServerRecvPubcomp(m)
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
    /\ resolution \in [Msgs -> {"none", "ack", "reject"}]
    /\ pubrecSent \in [Msgs -> BOOLEAN]
    /\ hasToken \in [Msgs -> BOOLEAN]
    /\ ackedEver \in [Msgs -> BOOLEAN]
    /\ deliveryCount \in [Msgs -> 0..2]

Resolved(m) == resolution[m] # "none"

(* Exactly-once DELIVERY. Holds when the client's dedup state survives the        *)
(* reconnect (transport reconnect). Under a process crash it is deliberately NOT  *)
(* asserted -- re-delivery is at-least-once PROCESSING, the documented contract.  *)
InvNoDoubleDelivery == \A m \in Msgs : deliveryCount[m] <= 1

(* NO PERMANENT WEDGE: a delivered id that the app can no longer resolve (no       *)
(* token, unresolved) while the broker still awaits its PUBREC is stuck forever    *)
(* -- the replay is suppressed by the dedup guard and no token is re-minted. This  *)
(* is unreachable when dedup state and token are lost together (crash) or kept     *)
(* together (transport); it is reachable ONLY when `delivered` is persisted but    *)
(* the token is not, which is why a design must not do that.                       *)
InvNoWedge ==
    \A m \in Msgs :
        ~(delivered[m] /\ ~hasToken[m] /\ resolution[m] = "none"
          /\ serverState[m] = "awaitPubrec")

(* DEFERRAL INTEGRITY (state bit and wire). *)
InvDeferralHeld == \A m \in Msgs : pubrecSent[m] => Resolved(m)
InvNoPubrecOnWireBeforeResolve ==
    \A p \in c2s : p.type = "pubrec" => Resolved(p.id)

(* TRUE at-least-once PROCESSING: the handshake advances past awaitPubrec only     *)
(* because the app genuinely ACKED (durable ackedEver), never via a fabrication.   *)
InvPubrelAfterResolve ==
    \A m \in Msgs : serverState[m] = "awaitPubcomp" => ackedEver[m]

(* Backpressure: outstanding QoS2 ids never exceed the Receive-Maximum window. *)
InvWindowBound == Cardinality(Outstanding) <= ReceiveMax

(* No ack without a delivery. *)
InvResolvedImpliesDelivered == \A m \in Msgs : Resolved(m) => delivered[m]

(* No dangling has_pubrec: a completed exchange (incl. a rejected one) owes no     *)
(* PUBREL.                                                                         *)
InvDoneImpliesNoPubrec == \A m \in Msgs : serverState[m] = "done" => ~pubrecSent[m]
============================================================================
