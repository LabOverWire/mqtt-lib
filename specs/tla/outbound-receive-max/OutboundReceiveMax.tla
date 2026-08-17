------------------------- MODULE OutboundReceiveMax -------------------------
(***************************************************************************)
(* Client-side OUTBOUND flow control: the client MUST NOT have more than    *)
(* the broker's advertised Receive Maximum (SRM) unacknowledged QoS1/QoS2    *)
(* PUBLISH packets outstanding at once (MQTT-5 section 4.9).                  *)
(*                                                                          *)
(* The bug this models: the client holds a send-quota permit per outbound    *)
(* QoS2 PUBLISH and must release it only when the message stops counting as  *)
(* "unacknowledged".  Per 4.9 a QoS2 message is unacknowledged for the WHOLE  *)
(* PUBLISH -> PUBREC -> PUBREL -> PUBCOMP exchange, so the permit is released  *)
(* on PUBCOMP (or on an error PUBREC with reason >= 0x80, which ends the      *)
(* exchange early).  Releasing on a SUCCESS PUBREC is the tempting-but-wrong  *)
(* choice: it frees a slot while PUBREL/PUBCOMP are still outstanding, so a    *)
(* fresh PUBLISH can push the true unacked count above SRM.                    *)
(*                                                                          *)
(* permitHeld  = the implementation's send-quota accounting (gates sends).    *)
(* SpecUnacked = the spec's definition of "unacknowledged" (sent, not yet     *)
(*               completed).  Safety requires Cardinality(SpecUnacked) <= SRM. *)
(* The gate uses permitHeld; the invariant is stated over SpecUnacked, so a    *)
(* release point that makes permitHeld diverge from SpecUnacked is caught.     *)
(*                                                                          *)
(* ReleaseOnPubrec = TRUE reproduces the bug; FALSE is the correct fix.        *)
(* Network is ordered and lossless at this stage -- adequate to expose the     *)
(* release-point defect; loss/dup/reconnect are later stages.                 *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    SRM,             \* broker-advertised Server Receive Maximum (the window)
    MaxMsgs,         \* total QoS2 messages the client will publish (bounds model)
    ReleaseOnPubrec  \* TRUE = buggy (free at PUBREC), FALSE = correct (free at PUBCOMP)

Msgs == 1..MaxMsgs

\* Per-message phase:
\*   "none"      not yet sent
\*   "published" PUBLISH sent, awaiting PUBREC
\*   "pubrec"    PUBREC received, PUBREL not yet sent
\*   "pubrel"    PUBREL sent, awaiting PUBCOMP
\*   "done"      PUBCOMP received (fully acknowledged) -- also used for error-PUBREC end
Phases == {"none", "published", "pubrec", "pubrel", "done"}

VARIABLES
    phase,       \* [Msgs -> Phases]
    permitHeld   \* subset of Msgs currently holding a send-quota permit

vars == <<phase, permitHeld>>

\* MQTT-5 4.9: a QoS2 message counts as unacknowledged from PUBLISH until PUBCOMP.
SpecUnacked == { m \in Msgs : phase[m] \in {"published", "pubrec", "pubrel"} }

Init ==
    /\ phase = [m \in Msgs |-> "none"]
    /\ permitHeld = {}

(* Client publishes m only while a quota permit is available (the gate). *)
Send(m) ==
    /\ phase[m] = "none"
    /\ Cardinality(permitHeld) < SRM
    /\ phase' = [phase EXCEPT ![m] = "published"]
    /\ permitHeld' = permitHeld \cup {m}

(* Client receives a SUCCESS PUBREC.  Buggy variant releases the permit here. *)
RecvPubrec(m) ==
    /\ phase[m] = "published"
    /\ phase' = [phase EXCEPT ![m] = "pubrec"]
    /\ permitHeld' = IF ReleaseOnPubrec THEN permitHeld \ {m} ELSE permitHeld

(* Client sends PUBREL. *)
SendPubrel(m) ==
    /\ phase[m] = "pubrec"
    /\ phase' = [phase EXCEPT ![m] = "pubrel"]
    /\ UNCHANGED permitHeld

(* Client receives PUBCOMP.  Correct variant releases the permit here. *)
RecvPubcomp(m) ==
    /\ phase[m] = "pubrel"
    /\ phase' = [phase EXCEPT ![m] = "done"]
    /\ permitHeld' = IF ReleaseOnPubrec THEN permitHeld ELSE permitHeld \ {m}

(* Client receives an ERROR PUBREC (reason >= 0x80): the QoS2 exchange ends,  *)
(* no PUBREL/PUBCOMP follow, and the permit is released regardless of variant. *)
RecvPubrecError(m) ==
    /\ phase[m] = "published"
    /\ phase' = [phase EXCEPT ![m] = "done"]
    /\ permitHeld' = permitHeld \ {m}

Terminating ==
    /\ \A m \in Msgs : phase[m] = "done"
    /\ UNCHANGED vars

Next ==
    \/ \E m \in Msgs : Send(m)
    \/ \E m \in Msgs : RecvPubrec(m)
    \/ \E m \in Msgs : SendPubrel(m)
    \/ \E m \in Msgs : RecvPubcomp(m)
    \/ \E m \in Msgs : RecvPubrecError(m)
    \/ Terminating

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ phase \in [Msgs -> Phases]
    /\ permitHeld \subseteq Msgs

(* The core MUST of 4.9: the true unacknowledged count never exceeds the      *)
(* broker's advertised window.  Violated by ReleaseOnPubrec = TRUE.           *)
InvWindowBound == Cardinality(SpecUnacked) <= SRM

(* Implementation accounting never itself exceeds the window.                 *)
InvPermitBound == Cardinality(permitHeld) <= SRM

(* In the correct model the permit accounting tracks the spec definition      *)
(* exactly -- no permit is released early and none is leaked.                  *)
InvPermitTracksUnacked == permitHeld = SpecUnacked
=============================================================================
