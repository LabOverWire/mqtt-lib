------------------------- MODULE DeferredAckReader -------------------------
(***************************************************************************)
(* Stage 2: the SINGLE READER TASK and the control-plane coupling.          *)
(*                                                                          *)
(* In the real client one reader task consumes the socket in order and both *)
(* (a) delivers inbound PUBLISH to the application, and (b) processes        *)
(* control packets (here: PINGRESP for keepalive; the same argument covers   *)
(* PUBACK/SUBACK for the client's own outbound ops).  Because it is ONE      *)
(* task reading ONE ordered socket, a PUBLISH it cannot make progress on     *)
(* head-of-line-blocks every control packet queued behind it.                *)
(*                                                                          *)
(* Two designs, selected by the boolean CONSTANT Blocking:                   *)
(*   Blocking = FALSE  (SAFE)   -- delivery to the app never blocks the       *)
(*        reader; backpressure is applied upstream by the Receive-Maximum     *)
(*        window (the server stops sending), so the reader always drains.     *)
(*   Blocking = TRUE   (UNSAFE) -- delivery is a bounded channel (capacity    *)
(*        Cap) that the reader must .await; a stalled consumer fills it and    *)
(*        the reader blocks, starving control packets.  This is #109 as        *)
(*        the author specified it ("apply its own backpressure on that         *)
(*        channel").                                                          *)
(*                                                                          *)
(* Liveness property checked: <>[](all pings processed).  The application     *)
(* (AppAck) is deliberately given NO fairness -- it models a consumer that     *)
(* may stall arbitrarily.  SAFE must still satisfy the property; UNSAFE must   *)
(* violate it, exhibiting the control-plane deadlock.                          *)
(*                                                                          *)
(* Abstraction note vs Stage 1: the PUBACK transit + ServerReceiveAck are     *)
(* collapsed into AppAck (it frees the server window directly).  Stage 2       *)
(* studies the reader/control-plane, not ack transit.                          *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    ReceiveMax,   \* inbound Receive-Maximum window (server-respected)
    MaxMsgs,      \* number of PUBLISH messages the server will produce
    Cap,          \* bounded app-channel capacity (only matters when Blocking)
    PingBudget,   \* number of PINGRESP control packets the server will send
    Blocking      \* TRUE = unsafe reader-blocking delivery; FALSE = safe

Msgs == 1..MaxMsgs
PubPacket  == [type: {"pub"}, id: Msgs]
PingPacket == [type: {"ping"}]
Packet == PubPacket \cup PingPacket

VARIABLES
    socket,         \* ordered packets in flight server -> client (the reader's input)
    toSend,         \* PUBLISH ids not yet put on the socket
    serverUnacked,  \* ids sent, not yet acked (server window view)
    pingsToSend,    \* remaining PINGRESP the server may send
    appBuf,         \* ids delivered to the app, not yet acked (outstanding tokens)
    delivered,      \* ids delivered to the app at least once
    ackedIds,       \* ids the app has acked
    pongsProcessed  \* count of PINGRESP the reader has processed (control progress)

vars == <<socket, toSend, serverUnacked, pingsToSend, appBuf, delivered,
          ackedIds, pongsProcessed>>

Init ==
    /\ socket = <<>>
    /\ toSend = Msgs
    /\ serverUnacked = {}
    /\ pingsToSend = PingBudget
    /\ appBuf = {}
    /\ delivered = {}
    /\ ackedIds = {}
    /\ pongsProcessed = 0

ServerSendPub(m) ==
    /\ m \in toSend
    /\ Cardinality(serverUnacked) < ReceiveMax
    /\ socket' = Append(socket, [type |-> "pub", id |-> m])
    /\ toSend' = toSend \ {m}
    /\ serverUnacked' = serverUnacked \cup {m}
    /\ UNCHANGED <<pingsToSend, appBuf, delivered, ackedIds, pongsProcessed>>

ServerSendPing ==
    /\ pingsToSend > 0
    /\ socket' = Append(socket, [type |-> "ping"])
    /\ pingsToSend' = pingsToSend - 1
    /\ UNCHANGED <<toSend, serverUnacked, appBuf, delivered, ackedIds, pongsProcessed>>

(* Reader processes a control packet at the head -- always non-blocking. *)
ReaderProcessPing ==
    /\ socket # <<>>
    /\ LET h == Head(socket) IN h.type = "ping"
    /\ socket' = Tail(socket)
    /\ pongsProcessed' = pongsProcessed + 1
    /\ UNCHANGED <<toSend, serverUnacked, pingsToSend, appBuf, delivered, ackedIds>>

(* Reader delivers a PUBLISH at the head.  When Blocking, this is DISABLED   *)
(* while the bounded app buffer is full -- modelling the reader parked on a   *)
(* .await it cannot complete, which blocks everything behind it on the socket.*)
ReaderDeliverPub ==
    /\ socket # <<>>
    /\ LET h == Head(socket) IN
         /\ h.type = "pub"
         /\ (~Blocking \/ Cardinality(appBuf) < Cap)
         /\ appBuf' = appBuf \cup {h.id}
         /\ delivered' = delivered \cup {h.id}
    /\ socket' = Tail(socket)
    /\ UNCHANGED <<toSend, serverUnacked, pingsToSend, ackedIds, pongsProcessed>>

(* The application acks a held message on its own schedule (deferred ack).    *)
(* NO fairness is attached to this action -- the consumer may stall forever.  *)
AppAck(m) ==
    /\ m \in appBuf
    /\ appBuf' = appBuf \ {m}
    /\ ackedIds' = ackedIds \cup {m}
    /\ serverUnacked' = serverUnacked \ {m}
    /\ UNCHANGED <<socket, toSend, pingsToSend, delivered, pongsProcessed>>

Complete == socket = <<>> /\ toSend = {} /\ appBuf = {} /\ pingsToSend = 0
Done == Complete /\ UNCHANGED vars

(* The consumer may stall arbitrarily: while it holds unacked tokens it can    *)
(* decline to act indefinitely.  This self-loop makes "consumer stalls         *)
(* forever" an admissible behaviour even when AppAck is the only other enabled *)
(* action, so the checker does not force the ack.  Reader/server weak fairness  *)
(* still compels THEIR progress whenever they are enabled, so SAFE is           *)
(* unaffected; only when the reader is blocked (UNSAFE) does idling win.        *)
ConsumerIdle == appBuf # {} /\ UNCHANGED vars

Next ==
    \/ \E m \in Msgs : ServerSendPub(m)
    \/ ServerSendPing
    \/ ReaderProcessPing
    \/ ReaderDeliverPub
    \/ \E m \in Msgs : AppAck(m)
    \/ ConsumerIdle
    \/ Done

(* Reader and server are always-running tasks (weak fairness).  The consumer  *)
(* (AppAck) is intentionally UNFAIR: liveness must not depend on it.           *)
Fairness ==
    /\ WF_vars(ReaderProcessPing)
    /\ WF_vars(ReaderDeliverPub)
    /\ WF_vars(ServerSendPing)
    /\ \A m \in Msgs : WF_vars(ServerSendPub(m))

Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
(* Safety invariants *)

TypeOK ==
    /\ \A i \in 1..Len(socket) : socket[i] \in Packet
    /\ toSend \subseteq Msgs
    /\ serverUnacked \subseteq Msgs
    /\ pingsToSend \in 0..PingBudget
    /\ appBuf \subseteq Msgs
    /\ delivered \subseteq Msgs
    /\ ackedIds \subseteq Msgs
    /\ pongsProcessed \in 0..PingBudget

InvServerWindowBound == Cardinality(serverUnacked) <= ReceiveMax
InvAppBufBound == Cardinality(appBuf) <= ReceiveMax
InvAckedDelivered == ackedIds \subseteq delivered

----------------------------------------------------------------------------
(* Control-plane starvation as a REACHABLE-STATE safety property.            *)
(*                                                                          *)
(* Kind(p) reads a packet's tag via an identifier (the parser rejects        *)
(* field access on a call result such as Head(socket).type).                 *)
Kind(p) == p.type

(* The reader is parked: a PUBLISH sits at the head that it cannot deliver    *)
(* because the bounded app buffer is full.  Only possible when Blocking.      *)
ReaderStuck ==
    /\ socket # <<>>
    /\ Kind(Head(socket)) = "pub"
    /\ Blocking
    /\ Cardinality(appBuf) >= Cap

(* ...and a control packet (PINGRESP) is stranded behind the parked PUBLISH.  *)
ControlStarved ==
    /\ ReaderStuck
    /\ \E i \in 1..Len(socket) : Kind(socket[i]) = "ping"

(* The head-of-line hazard: this configuration must be UNREACHABLE.  It holds *)
(* trivially under SAFE (Blocking=FALSE makes ReaderStuck false); under       *)
(* UNSAFE it is reachable, and the counterexample trace is the deadlock       *)
(* exhibit -- from here only the (unfair) consumer can free the control plane.*)
InvNoControlStarvation == ~ControlStarved

----------------------------------------------------------------------------
(* Every PINGRESP is eventually processed and stays processed, regardless of    *)
(* consumer behaviour.  Holds under SAFE, violated under UNSAFE.                *)
Liveness == <>[](pongsProcessed = PingBudget)
============================================================================
