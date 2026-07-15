------------------------- MODULE DeferredAckToken -------------------------
(***************************************************************************)
(* Stage 4b: AckToken DROP semantics.                                       *)
(*                                                                          *)
(* An AckToken owns a Receive-Maximum window slot for its lifetime.  The     *)
(* consumer may:                                                            *)
(*   - ack it (slot freed, PUBACK sent), or                                 *)
(*   - HOLD it indefinitely -- legitimate backpressure, NOT modelled as a    *)
(*     fault here (Stage 1 already shows a held token safely throttles the   *)
(*     server), or                                                          *)
(*   - DROP it without acking: the consumer abandons the token (early        *)
(*     return, `?`, panic, plain forgetfulness).  That is this stage.        *)
(*                                                                          *)
(* CONSTANT AutoAckOnDrop selects the Drop impl:                             *)
(*   FALSE -- naive: `Drop` does nothing.  The slot is never reclaimed; the   *)
(*        token is ABANDONED.  Expected: InvNoWedge VIOLATED.                *)
(*   TRUE  -- safe default: `Drop` emits the ack and frees the slot.          *)
(*        Expected: holds; `abandoned` stays empty by construction.           *)
(*                                                                          *)
(* The hazard being checked is NOT a crash: it is a SILENT PERMANENT WEDGE.  *)
(* Once abandoned tokens occupy the whole window, `Cardinality(inFlight) <    *)
(* ReceiveMax` is false forever, so the broker can never send again -- the    *)
(* subscription is dead while the process stays up and healthy.  Nothing in   *)
(* the model can free an abandoned slot, which is exactly the point.          *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    MaxMsgs,        \* messages the broker wants to deliver
    ReceiveMax,     \* inbound Receive-Maximum window
    AutoAckOnDrop   \* TRUE = Drop frees the slot; FALSE = naive Drop leaks it

Msgs == 1..MaxMsgs

VARIABLES
    toSend,     \* not yet delivered
    inFlight,   \* window slots occupied (register_inbound_publish / acknowledge_inbound)
    held,       \* tokens the consumer currently holds
    abandoned,  \* tokens dropped without acking -- slots that can never be reclaimed
    acked

vars == <<toSend, inFlight, held, abandoned, acked>>

Init ==
    /\ toSend = Msgs
    /\ inFlight = {}
    /\ held = {}
    /\ abandoned = {}
    /\ acked = {}

ServerSend(m) ==
    /\ m \in toSend
    /\ Cardinality(inFlight) < ReceiveMax
    /\ toSend' = toSend \ {m}
    /\ inFlight' = inFlight \cup {m}
    /\ held' = held \cup {m}
    /\ UNCHANGED <<abandoned, acked>>

(* Consumer acks: slot freed. *)
AppAck(m) ==
    /\ m \in held
    /\ held' = held \ {m}
    /\ inFlight' = inFlight \ {m}
    /\ acked' = acked \cup {m}
    /\ UNCHANGED <<toSend, abandoned>>

(* Safe Drop: the token's Drop impl emits the ack, so the slot is reclaimed.  *)
(* The message is acked without being processed (at-most-once for that one),  *)
(* but the window never leaks.                                               *)
AppDropAutoAck(m) ==
    /\ AutoAckOnDrop
    /\ m \in held
    /\ held' = held \ {m}
    /\ inFlight' = inFlight \ {m}
    /\ acked' = acked \cup {m}
    /\ UNCHANGED <<toSend, abandoned>>

(* Naive Drop: nothing happens.  The consumer no longer holds the token, but  *)
(* the window slot stays occupied forever.                                    *)
AppDropLeak(m) ==
    /\ ~AutoAckOnDrop
    /\ m \in held
    /\ held' = held \ {m}
    /\ abandoned' = abandoned \cup {m}
    /\ UNCHANGED <<toSend, inFlight, acked>>

Settled == toSend = {} /\ held = {}
Done == Settled /\ UNCHANGED vars

Next ==
    \/ \E m \in Msgs : ServerSend(m)
    \/ \E m \in Msgs : AppAck(m)
    \/ \E m \in Msgs : AppDropAutoAck(m)
    \/ \E m \in Msgs : AppDropLeak(m)
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ toSend \subseteq Msgs
    /\ inFlight \subseteq Msgs
    /\ held \subseteq Msgs
    /\ abandoned \subseteq Msgs
    /\ acked \subseteq Msgs

InvWindowBound == Cardinality(inFlight) <= ReceiveMax
InvHeldInFlight == held \subseteq inFlight
InvAbandonedInFlight == abandoned \subseteq inFlight

(* A held token is legitimate backpressure; an ABANDONED one is a leak.       *)
(* If abandoned tokens fill the window while work remains, delivery is dead   *)
(* forever: ServerSend's guard can never be satisfied again and no action can *)
(* free an abandoned slot.  This must be unreachable.                         *)
Wedged == Cardinality(abandoned) >= ReceiveMax /\ toSend # {}
InvNoWedge == ~Wedged
============================================================================
