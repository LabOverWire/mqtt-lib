------------------------ MODULE DeferredAckBidir -------------------------
(***************************************************************************)
(* Stage 6: BIDIRECTIONAL QoS2 and the shared-state collision.              *)
(*                                                                          *)
(* Why this exists: Stage 3 (DeferredAckQoS2.tla) models only the INBOUND   *)
(* direction, so it has no second packet-id space and could not represent   *)
(* — let alone find — the bug below. A quorum review found it by reading     *)
(* code. This stage closes that blind spot.                                 *)
(*                                                                          *)
(* The real defect: `unacked_pubrels` (session/state.rs:58) is ONE map used  *)
(* by BOTH QoS2 directions:                                                 *)
(*   - inbound  (we owe PUBREC):  mark_pubrec_pending / has_pubrec /         *)
(*                                remove_pubrec                              *)
(*   - outbound (we sent PUBREL): store_pubrel (state.rs:595, called from    *)
(*                                handle_pubrec_outgoing handlers.rs:274)    *)
(* MQTT packet ids are INDEPENDENT per direction and both allocators start   *)
(* at 1, so the two directions collide in one keyspace.                      *)
(*                                                                          *)
(* CONSTANT SharedMap selects the design:                                    *)
(*   TRUE  -- current code: the inbound dedup check consults a map that also  *)
(*        holds outbound PUBREL state.  Expected: InvNoSuppressedFirstDelivery*)
(*        VIOLATED — a legitimate inbound message is judged a duplicate and    *)
(*        silently dropped.  This is a MESSAGE-LOSS bug, worse than #112.      *)
(*   FALSE -- proposed fix: the inbound dedup owns its own map.  Expected: ok. *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    MaxId,            \* packet ids modelled (1 suffices: the collision is on the SAME id)
    MaxInboundSends,  \* bound on inbound PUBLISH transmissions per id (incl. DUP)
    SharedMap         \* TRUE = one map for both directions (current code)

Ids == 1..MaxId

VARIABLES
    outboundToPublish,       \* ids we may still publish at QoS2
    outboundAwaitingPubrec,  \* published, awaiting PUBREC
    pubrelKeys,              \* store_pubrel done, awaiting PUBCOMP  (OUTBOUND state)
    inboundKeys,             \* mark_pubrec_pending done, awaiting PUBREL (INBOUND state)
    inboundCompleted,        \* inbound handshake finished; the id is released
    inboundSends,            \* bounds the model
    deliveryCount,           \* times THIS inbound message reached the application
    wronglySuppressed        \* ids where a genuine first receipt was suppressed

vars == <<outboundToPublish, outboundAwaitingPubrec, pubrelKeys, inboundKeys,
          inboundCompleted, inboundSends, deliveryCount, wronglySuppressed>>

Init ==
    /\ outboundToPublish = Ids
    /\ outboundAwaitingPubrec = {}
    /\ pubrelKeys = {}
    /\ inboundKeys = {}
    /\ inboundCompleted = {}
    /\ inboundSends = [i \in Ids |-> 0]
    /\ deliveryCount = [i \in Ids |-> 0]
    /\ wronglySuppressed = {}

(* What the INBOUND dedup check actually consults.  With one shared map it    *)
(* also sees OUTBOUND PUBREL state — that is the bug.                         *)
EffectiveInbound ==
    IF SharedMap THEN inboundKeys \cup pubrelKeys ELSE inboundKeys

(* ---- outbound QoS2: we publish, broker PUBRECs, we store PUBREL ---- *)

OutboundPublish(id) ==
    /\ id \in outboundToPublish
    /\ outboundToPublish' = outboundToPublish \ {id}
    /\ outboundAwaitingPubrec' = outboundAwaitingPubrec \cup {id}
    /\ UNCHANGED <<pubrelKeys, inboundKeys, inboundCompleted, inboundSends,
                   deliveryCount, wronglySuppressed>>

OutboundRecvPubrec(id) ==
    /\ id \in outboundAwaitingPubrec
    /\ outboundAwaitingPubrec' = outboundAwaitingPubrec \ {id}
    /\ pubrelKeys' = pubrelKeys \cup {id}
    /\ UNCHANGED <<outboundToPublish, inboundKeys, inboundCompleted, inboundSends,
                   deliveryCount, wronglySuppressed>>

OutboundRecvPubcomp(id) ==
    /\ id \in pubrelKeys
    /\ pubrelKeys' = pubrelKeys \ {id}
    /\ UNCHANGED <<outboundToPublish, outboundAwaitingPubrec, inboundKeys,
                   inboundCompleted, inboundSends, deliveryCount, wronglySuppressed>>

(* ---- inbound QoS2: broker publishes to us ---- *)

(* One inbound message per id, possibly retransmitted (DUP) before its PUBREL. *)
(* Once the handshake completes the id is released; a later PUBLISH reusing it  *)
(* is a NEW message, so it SHOULD be delivered again. That is why this action   *)
(* is barred after completion rather than counted as a duplicate — conflating   *)
(* the two is what made the first cut of this model report a false violation.   *)
InboundPublish(id) ==
    /\ id \notin inboundCompleted
    /\ inboundSends[id] < MaxInboundSends
    /\ inboundSends' = [inboundSends EXCEPT ![id] = @ + 1]
    /\ LET genuinelyFirst == id \notin inboundKeys
           implFirst == id \notin EffectiveInbound
       IN
        /\ inboundKeys' = inboundKeys \cup {id}
        /\ deliveryCount' =
             IF implFirst
             THEN [deliveryCount EXCEPT ![id] = IF @ < 2 THEN @ + 1 ELSE @]
             ELSE deliveryCount
        /\ wronglySuppressed' =
             IF genuinelyFirst /\ ~implFirst
             THEN wronglySuppressed \cup {id}
             ELSE wronglySuppressed
    /\ UNCHANGED <<outboundToPublish, outboundAwaitingPubrec, pubrelKeys,
                   inboundCompleted>>

InboundPubrel(id) ==
    /\ id \in inboundKeys
    /\ inboundKeys' = inboundKeys \ {id}
    /\ inboundCompleted' = inboundCompleted \cup {id}
    /\ UNCHANGED <<outboundToPublish, outboundAwaitingPubrec, pubrelKeys,
                   inboundSends, deliveryCount, wronglySuppressed>>

Settled ==
    /\ outboundToPublish = {}
    /\ outboundAwaitingPubrec = {}
    /\ pubrelKeys = {}
    /\ inboundKeys = {}
    /\ inboundCompleted = Ids
Done == Settled /\ UNCHANGED vars

Next ==
    \/ \E id \in Ids : OutboundPublish(id)
    \/ \E id \in Ids : OutboundRecvPubrec(id)
    \/ \E id \in Ids : OutboundRecvPubcomp(id)
    \/ \E id \in Ids : InboundPublish(id)
    \/ \E id \in Ids : InboundPubrel(id)
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ outboundToPublish \subseteq Ids
    /\ outboundAwaitingPubrec \subseteq Ids
    /\ pubrelKeys \subseteq Ids
    /\ inboundKeys \subseteq Ids
    /\ inboundCompleted \subseteq Ids
    /\ inboundSends \in [Ids -> 0..MaxInboundSends]
    /\ deliveryCount \in [Ids -> 0..2]
    /\ wronglySuppressed \subseteq Ids

(* #112: a duplicate inbound PUBLISH must not reach the app twice. *)
InvNoDuplicateDelivery == \A id \in Ids : deliveryCount[id] <= 1

(* The NEW property, and the one the shared map violates: a genuine FIRST     *)
(* receipt must never be mistaken for a duplicate and dropped.  Suppressing a  *)
(* real message is silent data loss — strictly worse than delivering twice.    *)
InvNoSuppressedFirstDelivery == wronglySuppressed = {}
============================================================================
