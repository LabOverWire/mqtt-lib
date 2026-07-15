------------------------ MODULE DeferredAckSession ------------------------
(***************************************************************************)
(* Stage 4: what deferred acknowledgement actually guarantees across a       *)
(* crash + reconnect, and where it does NOT.                                *)
(*                                                                          *)
(* This pins the exact claim made to the author on issue #110: deferred ack  *)
(* buys at-least-once PROCESSING only when the session survives the          *)
(* reconnect, and the consumer must tolerate duplicates.                     *)
(*                                                                          *)
(* Model.  The app holds an ack token per delivered message; it acks ONLY    *)
(* after durably processing (that is the whole point of deferring).  A       *)
(* Disconnect models a client process crash: in-memory tokens are lost,      *)
(* durable `processed` state survives.  The broker's `serverUnacked` is its  *)
(* session state and survives the disconnect.  On Reconnect:                 *)
(*   SessionPersistent = TRUE  -- clean_start=false, session retained: the    *)
(*        broker keeps serverUnacked and will redeliver (DUP).               *)
(*   SessionPersistent = FALSE -- clean session: the broker DISCARDS          *)
(*        serverUnacked; anything delivered-but-unacked is gone forever.      *)
(*                                                                          *)
(* Expected results (checked as three separate runs so one violation does    *)
(* not mask the other):                                                      *)
(*   persistent + InvNoLoss                -> ok        (no message lost)     *)
(*   persistent + InvNoDuplicateProcessing -> VIOLATED  (duplicates possible) *)
(*   clean      + InvNoLoss                -> VIOLATED  (message lost)        *)
(* Together these are the formal statement of the tradeoff we documented.     *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    MaxMsgs,           \* distinct messages modelled
    ReceiveMax,        \* inbound Receive-Maximum window
    MaxSends,          \* bound on transmissions per message (initial + redeliveries)
    SessionPersistent  \* TRUE = clean_start=false (session survives reconnect)

Msgs == 1..MaxMsgs

VARIABLES
    toSend,          \* never yet sent by the broker
    serverUnacked,   \* broker session state: sent, awaiting PUBACK
    heldTokens,      \* client: ack tokens the app currently holds (in-memory)
    processedTokens, \* tokens whose message the app has durably processed
    processed,       \* durable: message was processed at least once
    processCount,    \* how many times the app processed it (duplicate detector)
    connected,
    sendCount        \* bounds the model

vars == <<toSend, serverUnacked, heldTokens, processedTokens, processed,
          processCount, connected, sendCount>>

Init ==
    /\ toSend = Msgs
    /\ serverUnacked = {}
    /\ heldTokens = {}
    /\ processedTokens = {}
    /\ processed = [m \in Msgs |-> FALSE]
    /\ processCount = [m \in Msgs |-> 0]
    /\ connected = TRUE
    /\ sendCount = [m \in Msgs |-> 0]

(* Initial delivery of a new message, or redelivery of one the broker still  *)
(* holds unacked (DUP after session resume).  New sends consume window room; *)
(* a redelivery does not, since the slot is already occupied.                *)
ServerSend(m) ==
    /\ connected
    /\ m \notin heldTokens
    /\ sendCount[m] < MaxSends
    /\ \/ (m \in toSend /\ Cardinality(serverUnacked) < ReceiveMax)
       \/ m \in serverUnacked
    /\ toSend' = toSend \ {m}
    /\ serverUnacked' = serverUnacked \cup {m}
    /\ heldTokens' = heldTokens \cup {m}
    /\ sendCount' = [sendCount EXCEPT ![m] = @ + 1]
    /\ UNCHANGED <<processedTokens, processed, processCount, connected>>

(* The app durably processes a held message.  This is what must happen BEFORE *)
(* the ack for deferred ack to mean anything.                                 *)
AppProcess(m) ==
    /\ m \in heldTokens
    /\ m \notin processedTokens
    /\ processedTokens' = processedTokens \cup {m}
    /\ processed' = [processed EXCEPT ![m] = TRUE]
    /\ processCount' = [processCount EXCEPT ![m] = IF @ < 2 THEN @ + 1 ELSE @]
    /\ UNCHANGED <<toSend, serverUnacked, heldTokens, connected, sendCount>>

(* Ack only after durable processing; the PUBACK frees the broker's slot. *)
AppAck(m) ==
    /\ connected
    /\ m \in processedTokens
    /\ heldTokens' = heldTokens \ {m}
    /\ processedTokens' = processedTokens \ {m}
    /\ serverUnacked' = serverUnacked \ {m}
    /\ UNCHANGED <<toSend, processed, processCount, connected, sendCount>>

(* Client process crash / connection loss: in-memory tokens evaporate.       *)
(* Durable `processed` survives; the broker's session state survives.        *)
Disconnect ==
    /\ connected
    /\ connected' = FALSE
    /\ heldTokens' = {}
    /\ processedTokens' = {}
    /\ UNCHANGED <<toSend, serverUnacked, processed, processCount, sendCount>>

(* Reconnect.  A clean session makes the broker discard everything it was    *)
(* holding unacked -- that is where deferred ack silently loses messages.    *)
Reconnect ==
    /\ ~connected
    /\ connected' = TRUE
    /\ serverUnacked' = IF SessionPersistent THEN serverUnacked ELSE {}
    /\ UNCHANGED <<toSend, heldTokens, processedTokens, processed,
                   processCount, sendCount>>

Settled == toSend = {} /\ serverUnacked = {} /\ heldTokens = {}
Done == Settled /\ UNCHANGED vars

Next ==
    \/ \E m \in Msgs : ServerSend(m)
    \/ \E m \in Msgs : AppProcess(m)
    \/ \E m \in Msgs : AppAck(m)
    \/ Disconnect
    \/ Reconnect
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ toSend \subseteq Msgs
    /\ serverUnacked \subseteq Msgs
    /\ heldTokens \subseteq Msgs
    /\ processedTokens \subseteq Msgs
    /\ processed \in [Msgs -> BOOLEAN]
    /\ processCount \in [Msgs -> 0..2]
    /\ connected \in BOOLEAN
    /\ sendCount \in [Msgs -> 0..MaxSends]

InvWindowBound == Cardinality(serverUnacked) <= ReceiveMax

(* A message is LOST when it has left the broker's hands for good (it will    *)
(* never be sent or redelivered) yet the app never processed it.              *)
Lost(m) ==
    /\ ~processed[m]
    /\ m \notin toSend
    /\ m \notin serverUnacked

InvNoLoss == \A m \in Msgs : ~Lost(m)

(* Exactly-once PROCESSING.  Deferred ack cannot provide this across a resumed *)
(* session: the crash-then-redeliver path reprocesses.  Expected to be         *)
(* violated when SessionPersistent -- this is the documented tradeoff, not a   *)
(* defect.                                                                     *)
InvNoDuplicateProcessing == \A m \in Msgs : processCount[m] <= 1
============================================================================
