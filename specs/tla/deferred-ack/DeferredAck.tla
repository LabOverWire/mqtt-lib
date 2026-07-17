---------------------------- MODULE DeferredAck ----------------------------
(***************************************************************************)
(* Stage 1: QoS1 inbound delivery with DEFERRED ACKNOWLEDGEMENT and a       *)
(* Receive-Maximum window.                                                  *)
(*                                                                          *)
(* A server streams QoS1 PUBLISH packets to a client.  The client's reader  *)
(* registers each message (taking a window slot) and delivers it to the     *)
(* application immediately, but the PUBACK is emitted later, on the         *)
(* application's own schedule (the "deferred ack" / capability-token model).*)
(* The MQTT Receive-Maximum window is the SOLE backpressure mechanism: the  *)
(* server will not send while its unacked count has reached ReceiveMax.     *)
(*                                                                          *)
(* Channels are modelled as SETS of message ids.  Each id is produced once  *)
(* by the server and acked once by the client, so no bag semantics are      *)
(* needed at this stage (network duplication/loss is added in Stage 3).     *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    ReceiveMax,   \* client-advertised inbound Receive-Maximum window size
    MaxMsgs       \* total messages the server will produce (bounds the model)

Msgs == 1..MaxMsgs

VARIABLES
    toSend,        \* subset of Msgs the server has not yet sent
    serverUnacked, \* ids sent by the server, not yet PUBACK'd (its window view)
    pubChan,       \* PUBLISH ids in flight server -> client
    ackChan,       \* PUBACK ids in flight client -> server
    clientWindow,  \* ids the client registered (ack token outstanding), not acked
    delivered,     \* ids handed to the application at least once
    ackedIds       \* ids for which the app has already emitted a PUBACK

vars == <<toSend, serverUnacked, pubChan, ackChan, clientWindow, delivered, ackedIds>>

Init ==
    /\ toSend = Msgs
    /\ serverUnacked = {}
    /\ pubChan = {}
    /\ ackChan = {}
    /\ clientWindow = {}
    /\ delivered = {}
    /\ ackedIds = {}

(* Server sends a PUBLISH only while its unacked window has room. *)
ServerSend(m) ==
    /\ m \in toSend
    /\ Cardinality(serverUnacked) < ReceiveMax
    /\ toSend' = toSend \ {m}
    /\ serverUnacked' = serverUnacked \cup {m}
    /\ pubChan' = pubChan \cup {m}
    /\ UNCHANGED <<ackChan, clientWindow, delivered, ackedIds>>

(* Reader receives a PUBLISH: takes a window slot and delivers to the app.  *)
(* Delivery and ack are separated in time -- this is the deferred model.    *)
ClientReceive(m) ==
    /\ m \in pubChan
    /\ pubChan' = pubChan \ {m}
    /\ clientWindow' = clientWindow \cup {m}
    /\ delivered' = delivered \cup {m}
    /\ UNCHANGED <<toSend, serverUnacked, ackChan, ackedIds>>

(* Application acks its token.  The token is single-shot: m must be in       *)
(* clientWindow and is removed, so the same id can never be acked twice.     *)
AppAck(m) ==
    /\ m \in clientWindow
    /\ clientWindow' = clientWindow \ {m}
    /\ ackChan' = ackChan \cup {m}
    /\ ackedIds' = ackedIds \cup {m}
    /\ UNCHANGED <<toSend, serverUnacked, pubChan, delivered>>

ServerReceiveAck(m) ==
    /\ m \in ackChan
    /\ ackChan' = ackChan \ {m}
    /\ serverUnacked' = serverUnacked \ {m}
    /\ UNCHANGED <<toSend, pubChan, clientWindow, delivered, ackedIds>>

(* Stutter at completion so the finished state is not a deadlock. *)
Terminating ==
    /\ toSend = {}
    /\ serverUnacked = {}
    /\ pubChan = {}
    /\ ackChan = {}
    /\ clientWindow = {}
    /\ UNCHANGED vars

Next ==
    \/ \E m \in Msgs : ServerSend(m)
    \/ \E m \in Msgs : ClientReceive(m)
    \/ \E m \in Msgs : AppAck(m)
    \/ \E m \in Msgs : ServerReceiveAck(m)
    \/ Terminating

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ toSend \subseteq Msgs
    /\ serverUnacked \subseteq Msgs
    /\ pubChan \subseteq Msgs
    /\ ackChan \subseteq Msgs
    /\ clientWindow \subseteq Msgs
    /\ delivered \subseteq Msgs
    /\ ackedIds \subseteq Msgs

(* Server never exceeds the advertised window. *)
InvServerWindowBound == Cardinality(serverUnacked) <= ReceiveMax

(* Client never holds more outstanding slots than the window allows. *)
InvClientWindowBound == Cardinality(clientWindow) <= ReceiveMax

(* An ack is only ever emitted for a message that was actually delivered. *)
InvNoAckWithoutDelivery == ackedIds \subseteq delivered

(* Every outstanding token corresponds to a delivered, not-yet-acked msg. *)
InvClientWindowDelivered == clientWindow \subseteq delivered

(* A held slot is never also already-acked (no orphan slot after ack). *)
InvNoAckedSlotHeld == clientWindow \cap ackedIds = {}
============================================================================
