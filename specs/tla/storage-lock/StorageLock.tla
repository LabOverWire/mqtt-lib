----------------------------- MODULE StorageLock -----------------------------
EXTENDS FiniteSets, TLC

CONSTANTS Conns, RELEASE, RUNWAIT, TIMEOUT

VARIABLES runState, h, holder, newRunning, exited, lost, clash

vars == <<runState, h, holder, newRunning, exited, lost, clash>>

AllWritten == \A c \in Conns : h[c] = "written"

TypeOK ==
    /\ runState \in {"running", "signalled", "returned"}
    /\ h \in [Conns -> {"open", "written"}]
    /\ holder \in {"old", "new", "none"}
    /\ newRunning \in BOOLEAN
    /\ exited \in BOOLEAN
    /\ lost \subseteq Conns
    /\ clash \in BOOLEAN

Init ==
    /\ runState = "running"
    /\ h = [c \in Conns |-> "open"]
    /\ holder = "old"
    /\ newRunning = FALSE
    /\ exited = FALSE
    /\ lost = {}
    /\ clash = FALSE

Signal ==
    /\ runState = "running"
    /\ runState' = "signalled"
    /\ UNCHANGED <<h, holder, newRunning, exited, lost, clash>>

HandlerWrite(c) ==
    /\ runState # "running"
    /\ ~exited
    /\ h[c] = "open"
    /\ h' = [h EXCEPT ![c] = "written"]
    /\ clash' = (clash \/ holder = "new")
    /\ UNCHANGED <<runState, holder, newRunning, exited, lost>>

Return ==
    /\ runState = "signalled"
    /\ \/ RUNWAIT = "NO"
       \/ AllWritten
    /\ runState' = "returned"
    /\ UNCHANGED <<h, holder, newRunning, exited, lost, clash>>

TimeoutReturn ==
    /\ TIMEOUT
    /\ RUNWAIT = "YES"
    /\ runState = "signalled"
    /\ ~AllWritten
    /\ runState' = "returned"
    /\ UNCHANGED <<h, holder, newRunning, exited, lost, clash>>

ReleaseAtShutdown ==
    /\ RELEASE = "SHUTDOWN"
    /\ runState # "running"
    /\ holder = "old"
    /\ holder' = "none"
    /\ UNCHANGED <<runState, h, newRunning, exited, lost, clash>>

ReleaseOnDrop ==
    /\ RELEASE = "DROP"
    /\ runState = "returned"
    /\ AllWritten
    /\ holder = "old"
    /\ holder' = "none"
    /\ UNCHANGED <<runState, h, newRunning, exited, lost, clash>>

ProcessExit ==
    /\ runState = "returned"
    /\ ~exited
    /\ exited' = TRUE
    /\ lost' = {c \in Conns : h[c] = "open"}
    /\ holder' = IF holder = "old" THEN "none" ELSE holder
    /\ UNCHANGED <<runState, h, newRunning, clash>>

NewStart ==
    /\ ~newRunning
    /\ holder = "none"
    /\ holder' = "new"
    /\ newRunning' = TRUE
    /\ UNCHANGED <<runState, h, exited, lost, clash>>

Next ==
    \/ Signal
    \/ \E c \in Conns : HandlerWrite(c)
    \/ Return
    \/ TimeoutReturn
    \/ ReleaseAtShutdown
    \/ ReleaseOnDrop
    \/ ProcessExit
    \/ NewStart

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(Signal)
    /\ \A c \in Conns : WF_vars(HandlerWrite(c))
    /\ WF_vars(Return)
    /\ WF_vars(ReleaseAtShutdown)
    /\ WF_vars(ReleaseOnDrop)
    /\ WF_vars(NewStart)

InvExclusive == ~clash

InvDurableAtReturn == runState = "returned" => AllWritten

InvNoLostWrites == lost = {}

NewEventuallyRuns == <>newRunning

NEG_ReturnedWithAllWrittenUnreachable == ~(runState = "returned" /\ AllWritten /\ ~exited)

Sym == Permutations(Conns)
=============================================================================
