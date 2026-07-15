-------------------------- MODULE DeferredAckQoS2 --------------------------
(***************************************************************************)
(* Stage 3: inbound QoS2 four-way handshake and the EXACTLY-ONCE DELIVERY   *)
(* guard (issue #112).                                                      *)
(*                                                                          *)
(* Flow modelled (client is the receiver):                                  *)
(*   server -> PUBLISH        client: register, deliver to app, send PUBREC *)
(*                                    and mark has_pubrec (unacked_pubrels) *)
(*   server -> PUBREL         client: clear has_pubrec, send PUBCOMP        *)
(*                                                                          *)
(* If the PUBREC never reaches the server (drop / connection loss), the      *)
(* server retransmits the PUBLISH with DUP=1.  MQTT-5 4.3.3 "Method A" says  *)
(* the receiver must re-send PUBREC but must NOT re-deliver the Application  *)
(* Message.                                                                  *)
(*                                                                          *)
(* CONSTANT Dedup selects the implementation:                                *)
(*   Dedup = FALSE -- current mqtt5 `main`: handle_publish_with_ack           *)
(*        dispatches unconditionally (handlers.rs:150), with no has_pubrec    *)
(*        check before delivery.  Expected: InvNoDoubleDelivery VIOLATED      *)
(*        (this is issue #112).                                              *)
(*   Dedup = TRUE  -- proposed fix: consult has_pubrec BEFORE delivering; on  *)
(*        a duplicate, re-send PUBREC and skip dispatch.  Expected: holds.    *)
(*                                                                          *)
(* The server->client channel `s2c` is an ORDERED sequence because MQTT runs  *)
(* over TCP: PUBREL can never overtake an earlier PUBLISH retransmission.     *)
(* Modelling it as an unordered set would admit a spurious counterexample     *)
(* (PUBREL processed first, clearing has_pubrec, then a stale DUP delivered). *)
(* Acks (c2s) are a set: their ordering is irrelevant to this property.       *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    MaxMsgs,   \* distinct packet ids modelled
    MaxSends,  \* bound on PUBLISH transmissions per id (initial + retransmits)
    Dedup      \* TRUE = has_pubrec guard before dispatch; FALSE = current main

Msgs == 1..MaxMsgs
S2CPacket == [type: {"pub", "pubrel"}, id: Msgs]
C2SPacket == [type: {"pubrec", "pubcomp"}, id: Msgs]

VARIABLES
    serverState,    \* per id: toSend -> awaitPubrec -> awaitPubcomp -> done
    sendCount,      \* per id: PUBLISH transmissions so far (bounds the model)
    s2c,            \* ORDERED server -> client packets (PUBLISH / PUBREL)
    c2s,            \* client -> server acks in flight (PUBREC / PUBCOMP)
    pubrecSent,     \* client's has_pubrec / unacked_pubrels marker
    deliveryCount   \* per id: times the Application Message reached the app

vars == <<serverState, sendCount, s2c, c2s, pubrecSent, deliveryCount>>

Init ==
    /\ serverState = [m \in Msgs |-> "toSend"]
    /\ sendCount = [m \in Msgs |-> 0]
    /\ s2c = <<>>
    /\ c2s = {}
    /\ pubrecSent = [m \in Msgs |-> FALSE]
    /\ deliveryCount = [m \in Msgs |-> 0]

(* Initial send, or a DUP retransmission while still awaiting PUBREC. *)
ServerSendPub(m) ==
    /\ serverState[m] \in {"toSend", "awaitPubrec"}
    /\ sendCount[m] < MaxSends
    /\ s2c' = Append(s2c, [type |-> "pub", id |-> m])
    /\ sendCount' = [sendCount EXCEPT ![m] = @ + 1]
    /\ serverState' = [serverState EXCEPT ![m] = "awaitPubrec"]
    /\ UNCHANGED <<c2s, pubrecSent, deliveryCount>>

(* Client receives a PUBLISH and DELIVERS it to the application.            *)
(* When Dedup is on, this is disabled for an id already marked has_pubrec.  *)
ClientRecvPubDeliver ==
    /\ s2c # <<>>
    /\ LET h == Head(s2c) IN
         /\ h.type = "pub"
         /\ ~(Dedup /\ pubrecSent[h.id])
         /\ deliveryCount' = [deliveryCount EXCEPT ![h.id] =
                                IF @ < 2 THEN @ + 1 ELSE @]
         /\ pubrecSent' = [pubrecSent EXCEPT ![h.id] = TRUE]
         /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> h.id]}
    /\ s2c' = Tail(s2c)
    /\ UNCHANGED <<serverState, sendCount>>

(* The fix: a DUP for an id we already PUBREC'd -- re-send PUBREC, do NOT    *)
(* deliver.  Only enabled when Dedup is on.                                  *)
ClientRecvPubSuppressed ==
    /\ s2c # <<>>
    /\ LET h == Head(s2c) IN
         /\ h.type = "pub"
         /\ Dedup
         /\ pubrecSent[h.id]
         /\ c2s' = c2s \cup {[type |-> "pubrec", id |-> h.id]}
    /\ s2c' = Tail(s2c)
    /\ UNCHANGED <<serverState, sendCount, pubrecSent, deliveryCount>>

ClientRecvPubrel ==
    /\ s2c # <<>>
    /\ LET h == Head(s2c) IN
         /\ h.type = "pubrel"
         /\ pubrecSent' = [pubrecSent EXCEPT ![h.id] = FALSE]
         /\ c2s' = c2s \cup {[type |-> "pubcomp", id |-> h.id]}
    /\ s2c' = Tail(s2c)
    /\ UNCHANGED <<serverState, sendCount, deliveryCount>>

ServerRecvPubrec(m) ==
    /\ [type |-> "pubrec", id |-> m] \in c2s
    /\ serverState[m] = "awaitPubrec"
    /\ c2s' = c2s \ {[type |-> "pubrec", id |-> m]}
    /\ serverState' = [serverState EXCEPT ![m] = "awaitPubcomp"]
    /\ s2c' = Append(s2c, [type |-> "pubrel", id |-> m])
    /\ UNCHANGED <<sendCount, pubrecSent, deliveryCount>>

ServerRecvPubcomp(m) ==
    /\ [type |-> "pubcomp", id |-> m] \in c2s
    /\ c2s' = c2s \ {[type |-> "pubcomp", id |-> m]}
    /\ serverState' = [serverState EXCEPT ![m] = "done"]
    /\ UNCHANGED <<s2c, sendCount, pubrecSent, deliveryCount>>

(* An ack fails to reach the server (packet drop / connection loss).  Losing *)
(* a PUBREC is what provokes the DUP PUBLISH retransmission.                 *)
LoseAck(p) ==
    /\ p \in c2s
    /\ c2s' = c2s \ {p}
    /\ UNCHANGED <<serverState, sendCount, s2c, pubrecSent, deliveryCount>>

AllDone == \A m \in Msgs : serverState[m] = "done"
Done == AllDone /\ UNCHANGED vars

Next ==
    \/ \E m \in Msgs : ServerSendPub(m)
    \/ ClientRecvPubDeliver
    \/ ClientRecvPubSuppressed
    \/ ClientRecvPubrel
    \/ \E m \in Msgs : ServerRecvPubrec(m)
    \/ \E m \in Msgs : ServerRecvPubcomp(m)
    \/ \E p \in C2SPacket : LoseAck(p)
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ serverState \in [Msgs -> {"toSend", "awaitPubrec", "awaitPubcomp", "done"}]
    /\ sendCount \in [Msgs -> 0..MaxSends]
    /\ \A i \in 1..Len(s2c) : s2c[i] \in S2CPacket
    /\ c2s \subseteq C2SPacket
    /\ pubrecSent \in [Msgs -> BOOLEAN]
    /\ deliveryCount \in [Msgs -> 0..2]

(* MQTT-5 4.3.3: the Application Message is delivered to the receiver        *)
(* EXACTLY ONCE.  Violated on current main (Dedup = FALSE) -- issue #112.    *)
InvNoDoubleDelivery == \A m \in Msgs : deliveryCount[m] <= 1

(* A message that completed the handshake must have been delivered. *)
InvDoneImpliesDelivered ==
    \A m \in Msgs : serverState[m] = "done" => deliveryCount[m] = 1
============================================================================
