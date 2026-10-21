---------------------------- MODULE FlowRecovery ----------------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS MaxStreams, STORE, FLAGS, DISCARD, LOOKUP, TEARDOWN

Conns == {"c1", "c2"}
Scopes == Conns \cup {"sess"}
Bits == {0, 1}
StreamIds == 1..MaxStreams
NoEntry == [st |-> "none", sid |-> 0, due |-> FALSE]
Entries == [st : {"none", "open", "kept", "closed", "fired"}, sid : 0..MaxStreams, due : BOOLEAN]
Streams == [c : Conns, clean : Bits, aif : Bits, exp : Bits,
            st : {"sent", "served", "refused", "fin", "rst", "ended", "gone"}]
Events == [sid : StreamIds, kind : {"closed", "discard"}]
AbsStates == [flow : {"none", "open", "kept"}, sid : 0..MaxStreams, gen : 0..MaxStreams,
              subs : SUBSET (1..MaxStreams)]

VARIABLES live, torn, owner, takenOver, streams, reg, subs, queue, abs,
          failed, ifailed, bad

vars == <<live, torn, owner, takenOver, streams, reg, subs, queue, abs,
          failed, ifailed, bad>>

TypeOK ==
    /\ live \in [Conns -> {"idle", "up", "down"}]
    /\ torn \in [Conns -> BOOLEAN]
    /\ owner \in Conns \cup {"none"}
    /\ takenOver \in [Conns -> BOOLEAN]
    /\ streams \in Seq(Streams)
    /\ Len(streams) <= MaxStreams
    /\ reg \in [Scopes -> Entries]
    /\ subs \subseteq 1..MaxStreams
    /\ queue \in [Conns -> Seq(Events)]
    /\ abs \in AbsStates
    /\ failed \in [Conns -> BOOLEAN]
    /\ ifailed \in [Conns -> BOOLEAN]

ScopeOf(c) == IF STORE = "SESSION" THEN "sess" ELSE c

Init ==
    /\ live = [c \in Conns |-> "idle"]
    /\ torn = [c \in Conns |-> FALSE]
    /\ owner = "none"
    /\ takenOver = [c \in Conns |-> FALSE]
    /\ streams = <<>>
    /\ reg = [s \in Scopes |-> NoEntry]
    /\ subs = {}
    /\ queue = [c \in Conns |-> <<>>]
    /\ abs = [flow |-> "none", sid |-> 0, gen |-> 0, subs |-> {}]
    /\ failed = [c \in Conns |-> FALSE]
    /\ ifailed = [c \in Conns |-> FALSE]
    /\ bad = {}

AbsDown(a, strs, c) ==
    IF a.flow = "open" /\ strs[a.sid].c = c
    THEN IF strs[a.sid].exp = 1
         THEN [a EXCEPT !.flow = "kept"]
         ELSE [a EXCEPT !.flow = "none", !.sid = 0, !.subs = {}]
    ELSE a

StreamsDown(strs, c) ==
    [j \in DOMAIN strs |->
        IF strs[j].c = c /\ strs[j].st \in {"sent", "served", "fin", "rst"}
        THEN [strs[j] EXCEPT !.st = "gone"]
        ELSE strs[j]]

ConnRegDown(e, strs, c) ==
    IF e.st = "open" /\ strs[e.sid].c = c
    THEN IF strs[e.sid].exp = 1
         THEN [e EXCEPT !.st = "kept"]
         ELSE [e EXCEPT !.st = "closed"]
    ELSE e

ConnQueueDown(q, e, strs, c) ==
    IF e.st = "open" /\ strs[e.sid].c = c /\ strs[e.sid].exp = 0
    THEN Append(q, [sid |-> e.sid, kind |-> "closed"])
    ELSE q

OnDown(e, strs, c) == e.st = "open" /\ strs[e.sid].c = c

Converted(e, strs, downs) ==
    IF e.st = "open" /\ strs[e.sid].c \in downs
    THEN IF strs[e.sid].exp = 1
         THEN [e EXCEPT !.st = "kept"]
         ELSE NoEntry
    ELSE e

ConvertDropsSubs(e, strs, downs) ==
    e.st = "open" /\ strs[e.sid].c \in downs /\ strs[e.sid].exp = 0

Down(c) ==
    /\ live' = [live EXCEPT ![c] = "down"]
    /\ streams' = StreamsDown(streams, c)
    /\ abs' = AbsDown(abs, streams, c)
    /\ IF STORE = "CONN"
       THEN /\ reg' = [reg EXCEPT ![c] = ConnRegDown(reg[c], streams, c)]
            /\ queue' = [queue EXCEPT ![c] = ConnQueueDown(queue[c], reg[c], streams, c)]
       ELSE UNCHANGED <<reg, queue>>

Open(c, cl, a, e) ==
    /\ live[c] = "up"
    /\ Len(streams) < MaxStreams
    /\ streams' = Append(streams, [c |-> c, clean |-> cl, aif |-> a, exp |-> e, st |-> "sent"])
    /\ UNCHANGED <<live, torn, owner, takenOver, reg, subs, queue, abs, failed, ifailed, bad>>

Verdict(s, has, fail) ==
    IF s.clean = 1 /\ s.aif = 1 THEN "discard"
    ELSE IF s.clean = 1 THEN "fresh"
    ELSE IF has THEN "recover"
    ELSE IF s.aif = 1 THEN (IF fail THEN "toomany" ELSE "nostate")
    ELSE "fresh"

Reply(v) == IF v \in {"fresh", "recover"} THEN "served" ELSE v

AbsAfter(v, i) ==
    CASE v = "discard" -> [abs EXCEPT !.flow = "none", !.sid = 0, !.subs = {}]
      [] v = "fresh" -> [abs EXCEPT !.flow = "open", !.sid = i, !.gen = abs.gen + 1, !.subs = {}]
      [] v = "recover" -> [abs EXCEPT !.flow = "open", !.sid = i]
      [] OTHER -> abs

ImplHas(e) ==
    /\ e.st \in {"open", "kept"}
    /\ ~(LOOKUP = "DEADLINE" /\ e.due)

ReaderOpen(i) ==
    LET s == streams[i]
        c == s.c
        sc == ScopeOf(c)
        e == reg[sc]
        expected == Verdict(s, abs.flow \in {"open", "kept"}, failed[c])
        actual == IF FLAGS = "IGNORE" THEN "fresh" ELSE Verdict(s, ImplHas(e), ifailed[c])
        absNext == AbsAfter(expected, i)
        servedSt == IF Reply(actual) = "served" THEN "served" ELSE "refused"
    IN
    /\ s.st = "sent"
    /\ live[c] = "up"
    /\ bad' = IF Reply(expected) = Reply(actual) THEN bad
              ELSE bad \cup {<<Reply(expected), Reply(actual)>>}
    /\ failed' = [failed EXCEPT ![c] = CASE expected = "nostate" -> TRUE
                                         [] s.clean = 1 -> FALSE
                                         [] OTHER -> failed[c]]
    /\ ifailed' = [ifailed EXCEPT ![c] = CASE actual = "nostate" -> TRUE
                                           [] s.clean = 1 -> FALSE
                                           [] OTHER -> ifailed[c]]
    /\ IF actual = "toomany"
       THEN /\ live' = [live EXCEPT ![c] = "down"]
            /\ streams' = StreamsDown([streams EXCEPT ![i].st = "refused"], c)
            /\ abs' = AbsDown(absNext, streams, c)
            /\ IF STORE = "CONN"
               THEN /\ reg' = [reg EXCEPT ![c] = ConnRegDown(reg[c], streams, c)]
                    /\ queue' = [queue EXCEPT ![c] = ConnQueueDown(queue[c], reg[c], streams, c)]
               ELSE UNCHANGED <<reg, queue>>
            /\ UNCHANGED subs
       ELSE /\ streams' = [streams EXCEPT ![i].st = servedSt]
            /\ abs' = absNext
            /\ UNCHANGED live
            /\ CASE actual \in {"discard", "fresh"} ->
                    /\ reg' = [reg EXCEPT ![sc] = IF actual = "fresh"
                                                  THEN [st |-> "open", sid |-> i, due |-> FALSE]
                                                  ELSE NoEntry]
                    /\ IF FLAGS = "IGNORE"
                       THEN UNCHANGED <<subs, queue>>
                       ELSE IF DISCARD = "ACKED"
                       THEN /\ subs' = {}
                            /\ UNCHANGED queue
                       ELSE /\ queue' = [queue EXCEPT ![c] = Append(queue[c], [sid |-> i, kind |-> "discard"])]
                            /\ UNCHANGED subs
                 [] actual = "recover" ->
                    /\ reg' = [reg EXCEPT ![sc] = [st |-> "open", sid |-> i, due |-> FALSE]]
                    /\ UNCHANGED <<subs, queue>>
                 [] OTHER ->
                    UNCHANGED <<reg, subs, queue>>
    /\ UNCHANGED <<torn, owner, takenOver>>

Subscribe(i) ==
    LET c == streams[i].c IN
    /\ streams[i].st = "served"
    /\ live[c] = "up"
    /\ owner = c
    /\ abs.flow = "open"
    /\ abs.sid = i
    /\ abs.gen \notin abs.subs
    /\ subs' = subs \cup {abs.gen}
    /\ abs' = [abs EXCEPT !.subs = abs.subs \cup {abs.gen}]
    /\ UNCHANGED <<live, torn, owner, takenOver, streams, reg, queue, failed, ifailed, bad>>

Finish(i) ==
    /\ streams[i].st = "served"
    /\ live[streams[i].c] = "up"
    /\ streams' = [streams EXCEPT ![i].st = "fin"]
    /\ UNCHANGED <<live, torn, owner, takenOver, reg, subs, queue, abs, failed, ifailed, bad>>

Reset(i) ==
    /\ streams[i].st = "served"
    /\ live[streams[i].c] = "up"
    /\ streams' = [streams EXCEPT ![i].st = "rst"]
    /\ UNCHANGED <<live, torn, owner, takenOver, reg, subs, queue, abs, failed, ifailed, bad>>

ReaderEnd(i) ==
    LET s == streams[i]
        c == s.c
        sc == ScopeOf(c)
        e == reg[sc]
        dropped == s.st = "fin" \/ s.exp = 0
    IN
    /\ s.st \in {"fin", "rst"}
    /\ live[c] = "up"
    /\ streams' = [streams EXCEPT ![i].st = "ended"]
    /\ abs' = IF abs.flow = "open" /\ abs.sid = i
              THEN IF dropped
                   THEN [abs EXCEPT !.flow = "none", !.sid = 0, !.subs = {}]
                   ELSE [abs EXCEPT !.flow = "kept"]
              ELSE abs
    /\ IF e.st = "open" /\ e.sid = i
       THEN IF STORE = "CONN"
            THEN IF dropped
                 THEN /\ reg' = [reg EXCEPT ![sc].st = "closed"]
                      /\ queue' = [queue EXCEPT ![c] = Append(queue[c], [sid |-> i, kind |-> "closed"])]
                      /\ UNCHANGED subs
                 ELSE /\ reg' = [reg EXCEPT ![sc].st = "kept"]
                      /\ UNCHANGED <<subs, queue>>
            ELSE IF dropped
                 THEN /\ reg' = [reg EXCEPT ![sc] = NoEntry]
                      /\ subs' = {}
                      /\ UNCHANGED queue
                 ELSE /\ reg' = [reg EXCEPT ![sc].st = "kept"]
                      /\ UNCHANGED <<subs, queue>>
       ELSE UNCHANGED <<reg, subs, queue>>
    /\ UNCHANGED <<live, torn, owner, takenOver, failed, ifailed, bad>>

HandleEvent(c) ==
    LET ev == Head(queue[c])
        sc == ScopeOf(c)
    IN
    /\ ~torn[c]
    /\ queue[c] # <<>>
    /\ queue' = [queue EXCEPT ![c] = Tail(queue[c])]
    /\ IF ev.kind = "closed"
       THEN IF reg[sc].st = "open"
            THEN UNCHANGED <<reg, subs>>
            ELSE /\ reg' = [reg EXCEPT ![sc] = NoEntry]
                 /\ subs' = IF owner = c THEN {} ELSE subs
       ELSE /\ subs' = IF owner = c THEN {} ELSE subs
            /\ UNCHANGED reg
    /\ UNCHANGED <<live, torn, owner, takenOver, streams, abs, failed, ifailed, bad>>

Tick ==
    LET sc == IF STORE = "SESSION" THEN "sess" ELSE streams[abs.sid].c
        e == reg[sc]
    IN
    /\ abs.flow = "kept"
    /\ abs' = [abs EXCEPT !.flow = "none", !.sid = 0, !.subs = {}]
    /\ reg' = IF e.sid = abs.sid /\ (e.st = "kept" \/ (e.st = "open" /\ live[streams[e.sid].c] = "down"))
              THEN [reg EXCEPT ![sc].due = TRUE]
              ELSE reg
    /\ UNCHANGED <<live, torn, owner, takenOver, streams, subs, queue, failed, ifailed, bad>>

TimerReady(sc) ==
    /\ reg[sc].st = "kept"
    /\ reg[sc].due
    /\ (STORE = "SESSION" \/ live[sc] = "up")

TimerFire(sc) ==
    LET e == reg[sc] IN
    /\ e.st = "kept"
    /\ e.due
    /\ IF STORE = "CONN"
       THEN /\ live[sc] = "up"
            /\ reg' = [reg EXCEPT ![sc].st = "fired"]
            /\ queue' = [queue EXCEPT ![sc] = Append(queue[sc], [sid |-> e.sid, kind |-> "closed"])]
            /\ UNCHANGED subs
       ELSE /\ reg' = [reg EXCEPT ![sc] = NoEntry]
            /\ subs' = {}
            /\ UNCHANGED queue
    /\ UNCHANGED <<live, torn, owner, takenOver, streams, abs, failed, ifailed, bad>>

Downs == {c \in Conns : live[c] = "down"}

Claim(c, cleanStart) ==
    LET old == owner
        takeover == old # "none" /\ live[old] = "up"
        strs1 == IF takeover THEN StreamsDown(streams, old) ELSE streams
        abs1 == IF takeover THEN AbsDown(abs, streams, old) ELSE abs
        downs1 == IF takeover THEN Downs \cup {old} ELSE Downs
        e == reg["sess"]
    IN
    /\ live[c] = "idle"
    /\ c = "c2" => live["c1"] # "idle"
    /\ live' = IF takeover THEN [live EXCEPT ![c] = "up", ![old] = "down"] ELSE [live EXCEPT ![c] = "up"]
    /\ takenOver' = IF takeover THEN [takenOver EXCEPT ![old] = TRUE] ELSE takenOver
    /\ owner' = c
    /\ streams' = strs1
    /\ abs' = IF cleanStart THEN [abs1 EXCEPT !.flow = "none", !.sid = 0, !.subs = {}] ELSE abs1
    /\ IF STORE = "CONN"
       THEN /\ reg' = IF takeover THEN [reg EXCEPT ![old] = ConnRegDown(reg[old], streams, old)] ELSE reg
            /\ queue' = IF takeover THEN [queue EXCEPT ![old] = ConnQueueDown(queue[old], reg[old], streams, old)] ELSE queue
            /\ subs' = IF cleanStart THEN {} ELSE subs
       ELSE /\ UNCHANGED queue
            /\ IF cleanStart
               THEN /\ reg' = [reg EXCEPT !["sess"] = NoEntry]
                    /\ subs' = {}
               ELSE IF TEARDOWN = "CLAIM"
               THEN /\ reg' = [reg EXCEPT !["sess"] = Converted(e, streams, downs1)]
                    /\ subs' = IF ConvertDropsSubs(e, streams, downs1) THEN {} ELSE subs
               ELSE UNCHANGED <<reg, subs>>
    /\ UNCHANGED <<torn, failed, ifailed, bad>>

Disconnect(c) ==
    /\ live[c] = "up"
    /\ Down(c)
    /\ UNCHANGED <<torn, owner, takenOver, subs, failed, ifailed, bad>>

DrainConn(c, q, e, sb) ==
    LET step(acc, ev) ==
            IF acc.e.st = "open" THEN acc
            ELSE [e |-> NoEntry, sb |-> IF owner = c THEN {} ELSE acc.sb]
        RECURSIVE fold(_, _)
        fold(acc, rest) == IF rest = <<>> THEN acc ELSE fold(step(acc, Head(rest)), Tail(rest))
    IN fold([e |-> e, sb |-> sb], q)

Teardown(c) ==
    /\ live[c] = "down"
    /\ ~torn[c]
    /\ torn' = [torn EXCEPT ![c] = TRUE]
    /\ queue' = [queue EXCEPT ![c] = <<>>]
    /\ IF STORE = "CONN"
       THEN LET drained == DrainConn(c, queue[c], reg[c], subs)
                unretained == \/ drained.e.st = "closed"
                              \/ drained.e.st = "open" /\ streams[drained.e.sid].exp = 0
            IN /\ reg' = [reg EXCEPT ![c] = NoEntry]
               /\ subs' = IF takenOver[c] THEN subs
                          ELSE IF unretained /\ owner = c THEN {}
                          ELSE drained.sb
       ELSE /\ IF owner = c
               THEN /\ reg' = [reg EXCEPT !["sess"] = Converted(reg["sess"], streams, Downs)]
                    /\ subs' = IF ConvertDropsSubs(reg["sess"], streams, Downs) THEN {} ELSE subs
               ELSE UNCHANGED <<reg, subs>>
    /\ UNCHANGED <<live, owner, takenOver, streams, abs, failed, ifailed, bad>>

Next ==
    \/ \E c \in Conns, cl \in Bits, a \in Bits, e \in Bits : Open(c, cl, a, e)
    \/ \E i \in 1..Len(streams) : ReaderOpen(i) \/ Subscribe(i) \/ Finish(i) \/ Reset(i) \/ ReaderEnd(i)
    \/ \E c \in Conns : HandleEvent(c) \/ Disconnect(c) \/ Teardown(c)
    \/ \E c \in Conns, cs \in BOOLEAN : Claim(c, cs)
    \/ Tick
    \/ \E sc \in Scopes : TimerFire(sc)

Spec == Init /\ [][Next]_vars

Pending ==
    \/ \E i \in 1..Len(streams) : streams[i].st \in {"sent", "fin", "rst"}
    \/ \E c \in Conns : ~torn[c] /\ queue[c] # <<>>
    \/ \E c \in Conns : live[c] = "down" /\ ~torn[c]
    \/ \E sc \in Scopes : TimerReady(sc)

InvResponses == bad = {}

InvNotEarly == abs.subs \subseteq subs

InvSettled == ~Pending => subs = abs.subs

NEG_KeptWithSubsUnreachable == ~(abs.subs # {} /\ abs.flow = "kept")

NEG_NoStateRefusalUnreachable == ~failed["c2"]

NEG_RecoveryAcrossConnectionsUnreachable ==
    ~(/\ abs.flow = "open"
      /\ abs.gen = 1
      /\ 1 \in subs
      /\ streams[abs.sid].c = "c2"
      /\ \E j \in 1..Len(streams) : streams[j].c = "c1" /\ streams[j].clean = 1)
=============================================================================
