---------------------------- MODULE OfflineQueue ----------------------------
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    N,
    K,
    MaxConns,
    RMSet,
    QSet,
    RSet,
    BSet,
    MQSet,
    RASet,
    MBSet,
    Design,
    Quarantine,
    Report,
    ReplayDowngrade

Msgs == 1..N
Ids == 1..K
Epochs == 1..MaxConns
Outcomes == {"none", "ok", "rejected", "indet"}
Pcs == {"none", "replay", "flush", "conformed", "popped", "stored", "done"}

Fix == Design = "Fix"

MaxOf(S) == CHOOSE x \in S : \A y \in S : x >= y
MinOf(S) == CHOOSE x \in S : \A y \in S : x <= y
Min(a, b) == IF a < b THEN a ELSE b

Full == [mq |-> 2, ra |-> TRUE, mb |-> TRUE, rm |-> MaxOf(RMSet)]
NoCur == [m |-> 0, id |-> 0, q |-> 0, re |-> FALSE]
IdleTask == [pc |-> "none", snap |-> <<>>, pol |-> Full, cur |-> NoCur, bad |-> FALSE]

KOk == [x \in Msgs |-> "ok"]
KRejected == [x \in Msgs |-> "rejected"]
KIndet == [x \in Msgs |-> "indet"]

VARIABLES
    reqQ, reqR, big,
    nextM, acc, outcome, outQ, effQ,
    callerMap, events, misattr,
    queue, store, task, quar, infl,
    up, conns, caps,
    wire, acks,
    rcount, rlog, srvQ2

reportVars == <<outcome, outQ, events>>
vars == <<reqQ, reqR, big, nextM, acc, outcome, outQ, effQ,
          callerMap, events, misattr, queue, store, task, quar, infl,
          up, conns, caps, wire, acks, rcount, rlog, srvQ2>>

Range(s) == {s[i] : i \in DOMAIN s}
MsgSet(s) == {s[i].m : i \in DOMAIN s}
RemoveMsg(s, m) == SelectSeq(s, LAMBDA x : x.m # m)
StoreDel(s, i) == SelectSeq(s, LAMBDA r : r.id # i)
StoreSet(s, i, f, v) ==
    [j \in DOMAIN s |-> IF s[j].id = i THEN [s[j] EXCEPT ![f] = v] ELSE s[j]]

CurTasks == {e \in Epochs : task[e].pc \in {"conformed", "popped", "stored"}}
PoppedTasks == {e \in Epochs : task[e].pc \in {"popped", "stored"}}
HeldCurs == {task[e].cur : e \in {x \in CurTasks : task[x].cur.q > 0}}

Holders ==
    {<<x.m, x.id>> : x \in Range(queue)}
    \cup {<<r.m, r.id>> : r \in Range(store)}
    \cup {<<c.m, c.id>> : c \in HeldCurs}

InUse ==
    {x.id : x \in Range(queue)}
    \cup {r.id : r \in Range(store)}
    \cup (IF Fix THEN {c.id : c \in HeldCurs} \cup quar ELSE {})

IsLive(m) ==
    \/ \E x \in Range(queue) : x.m = m
    \/ \E r \in Range(store) : r.m = m
    \/ \E e \in PoppedTasks : task[e].cur.m = m

IsPub(i) == wire[i].k = "pub"
OnWire(m) == \E i \in DOMAIN wire : wire[i].m = m
PendingEvent(m) == \E i \in DOMAIN events : events[i].m = m

QFor(x, mode) ==
    CASE mode = "eff" -> effQ[x]
      [] mode = "req" -> reqQ[x]
      [] OTHER -> 0

Resolve(s, kf, mode) ==
    IF Report = "handle"
    THEN /\ outcome' = [x \in Msgs |->
                          IF x \in MsgSet(s) /\ outcome[x] = "none" THEN kf[x] ELSE outcome[x]]
         /\ outQ' = [x \in Msgs |->
                       IF x \in MsgSet(s) /\ outcome[x] = "none" THEN QFor(x, mode) ELSE outQ[x]]
         /\ UNCHANGED events
    ELSE /\ events' = events \o [i \in 1..Len(s) |->
                                   [id |-> s[i].id, k |-> kf[s[i].m],
                                    q |-> QFor(s[i].m, mode), m |-> s[i].m]]
         /\ UNCHANGED <<outcome, outQ>>

PubPacket(m, id, q) == [k |-> "pub", m |-> m, id |-> IF q = 0 THEN 0 ELSE id, q |-> q, r |-> reqR[m]]
RelPacket(m, id) == [k |-> "rel", m |-> m, id |-> id, q |-> 2, r |-> FALSE]

Init ==
    /\ reqQ \in [Msgs -> QSet]
    /\ reqR \in [Msgs -> RSet]
    /\ big \in [Msgs -> BSet]
    /\ nextM = 1
    /\ acc = [m \in Msgs |-> FALSE]
    /\ outcome = [m \in Msgs |-> "none"]
    /\ outQ = [m \in Msgs |-> 0]
    /\ effQ = reqQ
    /\ callerMap = [i \in Ids |-> 0]
    /\ events = <<>>
    /\ misattr = FALSE
    /\ queue = <<>>
    /\ store = <<>>
    /\ task = [e \in Epochs |-> IdleTask]
    /\ quar = {}
    /\ infl = {}
    /\ up = FALSE
    /\ conns = 0
    /\ caps = Full
    /\ wire = <<>>
    /\ acks = <<>>
    /\ rcount = [m \in Msgs |-> 0]
    /\ rlog = <<>>
    /\ srvQ2 = {}

Accept ==
    /\ nextM <= N
    /\ LET m == nextM
           violatesLastKnown ==
               \/ reqR[m] /\ ~caps.ra /\ (Fix \/ up)
               \/ big[m] /\ ~caps.mb
           free == Ids \ InUse
       IN /\ nextM' = nextM + 1
          /\ IF violatesLastKnown \/ free = {}
               THEN /\ outcome' = [outcome EXCEPT ![m] = "rejected"]
                    /\ UNCHANGED <<acc, outQ, callerMap, queue, task>>
               ELSE LET p == MinOf(free) IN
                    /\ acc' = [acc EXCEPT ![m] = TRUE]
                    /\ queue' = Append(queue, [m |-> m, id |-> p, re |-> FALSE])
                    /\ callerMap' = IF Report = "pidEvent"
                                      THEN [callerMap EXCEPT ![p] = m]
                                      ELSE callerMap
                    /\ IF Design = "D0" /\ ~up
                         THEN /\ outcome' = [outcome EXCEPT ![m] = "ok"]
                              /\ outQ' = [outQ EXCEPT ![m] = reqQ[m]]
                         ELSE UNCHANGED <<outcome, outQ>>
                    /\ task' = IF up /\ task[conns].pc = "done"
                                 THEN [task EXCEPT ![conns].pc = "flush"]
                                 ELSE task
    /\ UNCHANGED <<reqQ, reqR, big, effQ, events, misattr, store, quar, infl,
                   up, conns, caps, wire, acks, rcount, rlog, srvQ2>>

Connect ==
    /\ ~up
    /\ conns < MaxConns
    /\ \E mq \in MQSet, ra \in RASet, mb \in MBSet, rm \in RMSet,
          sp \in (IF conns = 0 THEN {FALSE} ELSE BOOLEAN) :
         LET c == [mq |-> mq, ra |-> ra, mb |-> mb, rm |-> rm]
             e == conns + 1
             fresh == [pc |-> "replay",
                       snap |-> IF sp THEN [i \in 1..Len(store) |-> store[i].id] ELSE <<>>,
                       pol |-> c, cur |-> NoCur, bad |-> FALSE]
             qos1 == SelectSeq(store, LAMBDA r : r.q = 1)
             requeue == [i \in 1..Len(qos1) |-> [m |-> qos1[i].m, id |-> qos1[i].id, re |-> TRUE]]
             qos2 == SelectSeq(store, LAMBDA r : r.q = 2)
             owned == {r.m : r \in {x \in Range(store) : x.rel}}
             qos2Kind == [x \in Msgs |-> IF x \in owned THEN "ok" ELSE "indet"]
         IN /\ caps' = c
            /\ conns' = e
            /\ task' = [x \in Epochs |->
                          CASE x = e -> fresh
                            [] Fix -> IdleTask
                            [] OTHER -> task[x]]
            /\ IF sp
                 THEN UNCHANGED <<store, queue, srvQ2, quar, outcome, outQ, events>>
                 ELSE /\ srvQ2' = {}
                      /\ quar' = {}
                      /\ store' = <<>>
                      /\ IF Fix
                           THEN /\ queue' = requeue \o queue
                                /\ Resolve(qos2, qos2Kind, "eff")
                           ELSE /\ UNCHANGED queue
                                /\ Resolve(store, KIndet, "eff")
    /\ up' = TRUE
    /\ infl' = {}
    /\ UNCHANGED <<reqQ, reqR, big, nextM, acc, effQ, callerMap, misattr,
                   wire, acks, rcount, rlog>>

Lose ==
    /\ up
    /\ conns < MaxConns
    /\ up' = FALSE
    /\ wire' = <<>>
    /\ acks' = <<>>
    /\ infl' = {}
    /\ UNCHANGED <<reqQ, reqR, big, nextM, acc, outcome, outQ, effQ,
                   callerMap, events, misattr, queue, store, task, quar,
                   conns, caps, rcount, rlog, srvQ2>>

TaskFrame == <<reqQ, reqR, big, nextM, acc, callerMap, misattr, up, conns, caps,
               acks, rcount, rlog, srvQ2>>

SendReplay(e, r, q) ==
    /\ q > 0 => Cardinality(infl) < caps.rm
    /\ task' = [task EXCEPT ![e].snap = Tail(@)]
    /\ wire' = Append(wire, PubPacket(r.m, r.id, q))
    /\ effQ' = [effQ EXCEPT ![r.m] = Min(@, q)]
    /\ IF q = 0
         THEN /\ store' = StoreDel(store, r.id)
              /\ Resolve(<<r>>, KOk, "zero")
              /\ UNCHANGED infl
         ELSE /\ store' = StoreSet(store, r.id, "q", q)
              /\ infl' = infl \cup {r.id}
              /\ UNCHANGED reportVars
    /\ UNCHANGED quar

SendReplayRel(e, r) ==
    /\ Cardinality(infl) < caps.rm
    /\ task' = [task EXCEPT ![e].snap = Tail(@)]
    /\ wire' = Append(wire, RelPacket(r.m, r.id))
    /\ infl' = infl \cup {r.id}
    /\ UNCHANGED <<store, quar, effQ, outcome, outQ, events>>

AbandonReplay(e, r) ==
    /\ task' = [task EXCEPT ![e].snap = Tail(@)]
    /\ store' = StoreDel(store, r.id)
    /\ quar' = IF Quarantine /\ r.q = 2 THEN quar \cup {r.id} ELSE quar
    /\ Resolve(<<r>>, KIndet, "eff")
    /\ UNCHANGED <<wire, infl, effQ>>

ReplayStep(e) ==
    /\ task[e].pc = "replay"
    /\ task[e].snap # <<>>
    /\ e = conns
    /\ up
    /\ LET i == Head(task[e].snap)
           hits == {r \in Range(store) : r.id = i}
       IN IF hits = {}
          THEN /\ task' = [task EXCEPT ![e].snap = Tail(@)]
               /\ UNCHANGED <<store, wire, infl, quar, effQ, outcome, outQ, events>>
          ELSE LET r == CHOOSE x \in hits : TRUE
                   shapeOk == ~(reqR[r.m] /\ ~caps.ra) /\ ~(big[r.m] /\ ~caps.mb)
               IN CASE r.rel -> SendReplayRel(e, r)
                    [] Design = "D0" \/ (shapeOk /\ r.q <= caps.mq) -> SendReplay(e, r, r.q)
                    [] ReplayDowngrade /\ shapeOk -> SendReplay(e, r, caps.mq)
                    [] OTHER -> AbandonReplay(e, r)
    /\ UNCHANGED queue
    /\ UNCHANGED TaskFrame

ReplayEnd(e) ==
    /\ task[e].pc = "replay"
    /\ task[e].snap = <<>>
    /\ task' = [task EXCEPT ![e].pc = "flush"]
    /\ UNCHANGED <<queue, store, wire, infl, quar, effQ, outcome, outQ, events>>
    /\ UNCHANGED TaskFrame

ReplayDie(e) ==
    /\ task[e].pc = "replay"
    /\ task[e].snap # <<>>
    /\ e # conns \/ ~up
    /\ task' = [task EXCEPT ![e] = IdleTask]
    /\ UNCHANGED <<queue, store, wire, infl, quar, effQ, outcome, outQ, events>>
    /\ UNCHANGED TaskFrame

FlushFinish(e) ==
    /\ task[e].pc = "flush"
    /\ queue = <<>>
    /\ task' = [task EXCEPT ![e].pc = "done"]
    /\ UNCHANGED <<queue, store, wire, infl, quar, effQ, outcome, outQ, events>>
    /\ UNCHANGED TaskFrame

Conform(e) ==
    /\ task[e].pc = "flush"
    /\ queue # <<>>
    /\ Fix => (e = conns /\ up)
    /\ LET h == Head(queue)
           pol == task[e].pol
       IN task' = [task EXCEPT ![e].pc = "conformed",
                               ![e].cur = [m |-> h.m, id |-> h.id,
                                           q |-> Min(effQ[h.m], pol.mq), re |-> h.re],
                               ![e].bad = (reqR[h.m] /\ ~pol.ra) \/ (big[h.m] /\ ~pol.mb)]
    /\ UNCHANGED <<queue, store, wire, infl, quar, effQ, outcome, outQ, events>>
    /\ UNCHANGED TaskFrame

Drop(e) ==
    /\ task[e].pc = "conformed"
    /\ task[e].bad
    /\ Fix => e = conns
    /\ LET c == task[e].cur IN
       IF Fix
       THEN /\ queue' = RemoveMsg(queue, c.m)
            /\ Resolve(<<c>>, IF c.re THEN KIndet ELSE KRejected, "eff")
       ELSE /\ queue' = IF queue = <<>> THEN queue ELSE Tail(queue)
            /\ UNCHANGED reportVars
    /\ task' = [task EXCEPT ![e].pc = "flush", ![e].cur = NoCur, ![e].bad = FALSE]
    /\ UNCHANGED <<store, wire, infl, quar, effQ>>
    /\ UNCHANGED TaskFrame

ClaimFix(e) ==
    /\ e = conns
    /\ up
    /\ LET p == task[e].cur IN
       /\ p.q > 0 => Cardinality(infl) < caps.rm
       /\ queue' = RemoveMsg(queue, p.m)
       /\ wire' = Append(wire, PubPacket(p.m, p.id, p.q))
       /\ effQ' = [effQ EXCEPT ![p.m] = Min(@, p.q)]
       /\ task' = [task EXCEPT ![e].pc = "flush", ![e].cur = NoCur]
       /\ IF p.q = 0
            THEN /\ Resolve(<<p>>, KOk, "zero")
                 /\ UNCHANGED <<store, infl>>
            ELSE /\ store' = Append(store, [m |-> p.m, id |-> p.id, q |-> p.q, rel |-> FALSE])
                 /\ infl' = infl \cup {p.id}
                 /\ UNCHANGED reportVars

ClaimD0(e) ==
    /\ LET p == task[e].cur IN
       /\ p.q > 0 => (e = conns /\ Cardinality(infl) < caps.rm)
       /\ queue' = IF queue = <<>> THEN queue ELSE Tail(queue)
       /\ infl' = IF p.q = 0 THEN infl ELSE infl \cup {p.id}
       /\ task' = [task EXCEPT ![e].pc = "popped"]
       /\ UNCHANGED <<wire, effQ, store, outcome, outQ, events>>

Claim(e) ==
    /\ task[e].pc = "conformed"
    /\ ~task[e].bad
    /\ IF Fix THEN ClaimFix(e) ELSE ClaimD0(e)
    /\ UNCHANGED quar
    /\ UNCHANGED TaskFrame

StoreStep(e) ==
    /\ task[e].pc = "popped"
    /\ LET p == task[e].cur IN
       store' = IF p.q > 0
                  THEN Append(StoreDel(store, p.id), [m |-> p.m, id |-> p.id, q |-> p.q, rel |-> FALSE])
                  ELSE store
    /\ task' = [task EXCEPT ![e].pc = "stored"]
    /\ UNCHANGED <<queue, wire, infl, quar, effQ, outcome, outQ, events>>
    /\ UNCHANGED TaskFrame

WriteStep(e) ==
    /\ task[e].pc = "stored"
    /\ LET p == task[e].cur IN
       IF e = conns /\ up
       THEN /\ wire' = Append(wire, PubPacket(p.m, p.id, p.q))
            /\ effQ' = [effQ EXCEPT ![p.m] = Min(@, p.q)]
            /\ task' = [task EXCEPT ![e].pc = "flush", ![e].cur = NoCur]
            /\ IF p.q = 0 THEN Resolve(<<p>>, KOk, "zero") ELSE UNCHANGED reportVars
       ELSE /\ task' = [task EXCEPT ![e] = IdleTask]
            /\ UNCHANGED <<wire, effQ, outcome, outQ, events>>
    /\ UNCHANGED <<queue, store, infl, quar>>
    /\ UNCHANGED TaskFrame

TaskStep(e) ==
    \/ ReplayStep(e) \/ ReplayEnd(e) \/ ReplayDie(e)
    \/ FlushFinish(e) \/ Conform(e) \/ Drop(e) \/ Claim(e)
    \/ StoreStep(e) \/ WriteStep(e)

TaskAny == \E e \in Epochs : TaskStep(e)

ServerRecv ==
    /\ up
    /\ wire # <<>>
    /\ LET p == Head(wire)
           dedup == p.q = 2 /\ \E pr \in srvQ2 : pr[1] = p.id
       IN /\ wire' = Tail(wire)
          /\ IF p.k = "rel"
               THEN /\ srvQ2' = {pr \in srvQ2 : pr[1] # p.id}
                    /\ acks' = Append(acks, [k |-> "comp", id |-> p.id])
                    /\ UNCHANGED <<rcount, rlog>>
               ELSE /\ rcount' = IF dedup THEN rcount ELSE [rcount EXCEPT ![p.m] = Min(@ + 1, 2)]
                    /\ rlog' = IF ~dedup /\ rcount[p.m] = 0 THEN Append(rlog, p.m) ELSE rlog
                    /\ srvQ2' = IF p.q = 2 /\ ~dedup THEN srvQ2 \cup {<<p.id, p.m>>} ELSE srvQ2
                    /\ acks' = CASE p.q = 2 -> Append(acks, [k |-> "rec", id |-> p.id])
                                 [] p.q = 1 -> Append(acks, [k |-> "ack", id |-> p.id])
                                 [] OTHER -> acks
    /\ UNCHANGED <<reqQ, reqR, big, nextM, acc, outcome, outQ, effQ,
                   callerMap, events, misattr, queue, store, task, quar, infl,
                   up, conns, caps>>

ClientAck ==
    /\ up
    /\ acks # <<>>
    /\ LET a == Head(acks)
           hits == {r \in Range(store) : r.id = a.id}
       IN /\ acks' = Tail(acks)
          /\ IF hits = {}
               THEN UNCHANGED <<store, infl, wire, outcome, outQ, events>>
               ELSE LET r == CHOOSE x \in hits : TRUE IN
                    IF a.k = "rec"
                    THEN /\ store' = StoreSet(store, a.id, "rel", TRUE)
                         /\ wire' = Append(wire, RelPacket(r.m, a.id))
                         /\ UNCHANGED <<infl, outcome, outQ, events>>
                    ELSE /\ store' = StoreDel(store, a.id)
                         /\ infl' = infl \ {a.id}
                         /\ Resolve(<<r>>, KOk, IF Fix THEN "eff" ELSE "req")
                         /\ UNCHANGED wire
    /\ UNCHANGED <<reqQ, reqR, big, nextM, acc, effQ, callerMap, misattr,
                   queue, task, quar, up, conns, caps, rcount, rlog, srvQ2>>

CallerConsume ==
    /\ events # <<>>
    /\ LET ev == Head(events)
           t == callerMap[ev.id]
       IN /\ events' = Tail(events)
          /\ outcome' = IF outcome[t] = "none" THEN [outcome EXCEPT ![t] = ev.k] ELSE outcome
          /\ outQ' = IF outcome[t] = "none" THEN [outQ EXCEPT ![t] = ev.q] ELSE outQ
          /\ misattr' = (misattr \/ t # ev.m)
    /\ UNCHANGED <<reqQ, reqR, big, nextM, acc, effQ, callerMap, queue, store,
                   task, quar, infl, up, conns, caps, wire, acks, rcount, rlog, srvQ2>>

Next ==
    \/ Accept
    \/ Connect
    \/ Lose
    \/ TaskAny
    \/ ServerRecv
    \/ ClientAck
    \/ CallerConsume

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(Connect)
    /\ WF_vars(TaskAny)
    /\ WF_vars(ServerRecv)
    /\ WF_vars(ClientAck)
    /\ WF_vars(CallerConsume)

SpecNoConnectFairness ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(TaskAny)
    /\ WF_vars(ServerRecv)
    /\ WF_vars(ClientAck)
    /\ WF_vars(CallerConsume)

----------------------------------------------------------------------------

TypeOK ==
    /\ reqQ \in [Msgs -> 1..2]
    /\ nextM \in 1..(N + 1)
    /\ acc \in [Msgs -> BOOLEAN]
    /\ outcome \in [Msgs -> Outcomes]
    /\ outQ \in [Msgs -> 0..2]
    /\ effQ \in [Msgs -> 0..2]
    /\ callerMap \in [Ids -> 0..N]
    /\ Len(events) <= N
    /\ misattr \in BOOLEAN
    /\ Len(queue) <= N
    /\ Len(store) <= N
    /\ \A e \in Epochs : task[e].pc \in Pcs
    /\ quar \subseteq Ids
    /\ infl \subseteq Ids
    /\ up \in BOOLEAN
    /\ conns \in 0..MaxConns
    /\ Len(wire) <= 3 * N
    /\ rcount \in [Msgs -> 0..2]
    /\ Len(rlog) <= N

InvRetainAvailable == \A i \in DOMAIN wire : (IsPub(i) /\ wire[i].r) => caps.ra

InvMaximumQoS == \A i \in DOMAIN wire : IsPub(i) => wire[i].q <= caps.mq

InvMaximumPacketSize == \A i \in DOMAIN wire : (IsPub(i) /\ big[wire[i].m]) => caps.mb

InvReceiveMaximum == Cardinality(infl) <= caps.rm

InvNoSilentLoss ==
    \A m \in Msgs :
        (acc[m] /\ ~IsLive(m) /\ rcount[m] = 0 /\ ~OnWire(m)) =>
            \/ outcome[m] \in {"rejected", "indet"}
            \/ outcome[m] = "ok" /\ outQ[m] = 0
            \/ PendingEvent(m)

InvRejectedNeverDelivered ==
    \A m \in Msgs : outcome[m] = "rejected" => (rcount[m] = 0 /\ ~OnWire(m))

InvOrder == \A i, j \in DOMAIN rlog : i < j => rlog[i] < rlog[j]

InvPidUnique == \A a, b \in Holders : a[2] = b[2] => a[1] = b[1]

InvPidNotQuarantined == \A h \in Holders : h[2] \notin quar

InvNoStaleServerPid ==
    /\ \A i \in DOMAIN wire : \A pr \in srvQ2 :
          pr[1] = wire[i].id => (pr[2] = wire[i].m /\ wire[i].q = 2)
    /\ \A h \in Holders : \A pr \in srvQ2 : pr[1] = h[2] => pr[2] = h[1]

InvNoMisattribution == ~misattr

InvExactlyOnce ==
    \A m \in Msgs :
        rcount[m] > 1 =>
            /\ effQ[m] < 2
            /\ outcome[m] \in {"ok", "indet"} => outQ[m] < 2

InvRetainFidelity == \A i \in DOMAIN wire : IsPub(i) => wire[i].r = reqR[wire[i].m]

InvQoSFidelity == \A m \in Msgs : outcome[m] \in {"ok", "indet"} => outQ[m] = effQ[m]

Unresolved == \E m \in Msgs : acc[m] /\ outcome[m] = "none"

InvFairActionEnabled ==
    Unresolved => ENABLED (Connect \/ TaskAny \/ ServerRecv \/ ClientAck \/ CallerConsume)

AllResolved == \A m \in Msgs : acc[m] => outcome[m] # "none"

Settled == queue = <<>> /\ store = <<>>

LiveResolved == []<>AllResolved

LiveSettled == []<>Settled

NegAllDelivered == []<>(\A m \in Msgs : acc[m] => outcome[m] = "ok")

NegQuarantineReleased == []<>(quar = {})

NegImpossible == []<>(nextM > N + 1)
=============================================================================
