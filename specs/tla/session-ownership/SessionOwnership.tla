--------------------------- MODULE SessionOwnership ---------------------------
EXTENDS Integers, FiniteSets, Sequences, TLC

CONSTANTS Conns, Subs, None, MaxT, E, MaxBoots, MECH, DROP, GC, MaxPend

VARIABLES pc, tok, clr, ex, loc, pend, cborn, startEff,
          spOK, linOK, subOK, abortOK, failed,
          owner, hwm, rsubs, rsPend, swPend, nextId,
          cache, disk, dirty,
          now, boots, bootT,
          gEx, gEnd, gExp, gOwner, gSubs, cleanEff,
          pbuf, dg, dpos, wpos, ackSafe

connV   == <<pc, tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed>>
routerV == <<owner, hwm, rsubs, rsPend, swPend, nextId>>
storeV  == <<cache, disk, dirty>>
timeV   == <<now, boots, bootT>>
ghostV  == <<gEx, gEnd, gExp, gOwner, gSubs, cleanEff>>
gcV     == <<pbuf, dg, dpos, wpos, ackSafe>>
vars    == <<connV, routerV, storeV, timeV, ghostV, gcV>>

LockFixes == {"claim", "sub", "release", "disc", "sweep", "stamp", "rebuild",
              "wt_claim", "wt_sub", "wt_disc", "wt_remove"}

FixSet ==
  CASE MECH = "LOCK"     -> LockFixes \ DROP
    [] MECH = "CURRENT"  -> {"wt_remove"}
    [] MECH = "CAS"      -> {"disc", "stamp", "rebuild", "wt_claim", "wt_sub", "wt_disc", "wt_remove"}
    [] MECH = "CASSPLIT" -> {"disc", "stamp", "rebuild", "wt_claim", "wt_sub", "wt_disc", "wt_remove"}

On(f) == f \in FixSet

IsLock  == MECH = "LOCK"
IsCas   == MECH \in {"CAS", "CASSPLIT"}
Legacy(f) == MECH \in {"LOCK", "CURRENT"} /\ ~On(f)

MaxTok == Cardinality(Conns) + 1

NoRec  == [ex |-> FALSE, tok |-> 0, subs |-> {}, exp |-> 0, conn |-> FALSE, seen |-> 0,
           gdt |-> 0, born |-> 0]
NoDisk == [ex |-> FALSE, tok |-> 0, subs |-> {}, exp |-> 0, ts |-> -1, gdt |-> 0, born |-> 0]

Pcs == {"idle", "store", "hconnack", "register", "install",
        "cstore", "cinstall", "creg", "connack",
        "live", "substore", "unsubstore", "suback", "relwait",
        "release", "strip", "cleanup", "rmowned", "failrel", "cclean",
        "done", "dead"}
Closing == {"release", "strip", "cleanup", "rmowned", "failrel", "cclean"}
LivePC  == {"live", "substore", "unsubstore", "suback"}
Ended   == {"idle", "done", "dead"}

Dur(e) == IF e = 0 THEN 0 ELSE E

Expired(r) == r.ex /\ ~r.conn /\ now > r.seen + Dur(r.exp)

Load(d) == [ex |-> TRUE, tok |-> d.tok, subs |-> d.subs, exp |-> d.exp, conn |-> FALSE,
            seen |-> IF On("stamp") THEN (IF d.ts = -1 THEN bootT ELSE d.ts) ELSE now,
            gdt |-> d.gdt, born |-> d.born]

ToDisk(r) == [ex |-> TRUE, tok |-> r.tok, subs |-> r.subs, exp |-> r.exp,
              ts |-> IF On("stamp") /\ ~r.conn THEN r.seen ELSE -1,
              gdt |-> r.gdt, born |-> r.born]

View == IF cache.ex \/ dirty THEN cache ELSE IF disk.ex THEN Load(disk) ELSE NoRec

DiskView ==
  IF disk.ex
  THEN [ex |-> TRUE, tok |-> disk.tok, subs |-> disk.subs, exp |-> disk.exp,
        conn |-> disk.ts = -1, seen |-> IF disk.ts = -1 THEN now ELSE disk.ts,
        gdt |-> disk.gdt, born |-> disk.born]
  ELSE NoRec

RV == IF GC = "NORYW" THEN DiskView ELSE View

ReadExpired == RV.ex /\ Expired(RV)
ReadRes == IF ReadExpired THEN NoRec ELSE RV
DiskAfterRead == IF ReadExpired /\ On("wt_remove") THEN NoDisk ELSE disk

Wr(r, wt, d0) ==
  /\ cache' = r
  /\ IF wt THEN IF GC = "OFF" THEN disk' = ToDisk(r) /\ dirty' = FALSE
                ELSE disk' = disk /\ dirty' = TRUE
           ELSE disk' = d0 /\ dirty' = TRUE

Remove(wt) ==
  /\ cache' = NoRec
  /\ IF wt THEN IF GC = "OFF" THEN disk' = NoDisk /\ dirty' = FALSE
                ELSE disk' = disk /\ dirty' = TRUE
           ELSE disk' = disk /\ dirty' = TRUE

ExpireOnRead == IF ReadExpired THEN Remove(On("wt_remove")) ELSE UNCHANGED storeV

Owns(c) == tok[c] # 0 /\ RV.ex /\ RV.tok = tok[c]

OwnsNow(c) == tok[c] # 0 /\ View.ex /\ View.tok = tok[c]

Holder(t) == \E c \in Conns : pc[c] \notin Ended /\ tok[c] = t

GAlive == gEx /\ gExp > 0 /\ (gEnd = -1 \/ now <= gEnd + E)

GhostEnd(c, e) ==
  IF gOwner = c
  THEN /\ gEnd' = now /\ gExp' = e /\ gEx' = (gEx /\ e > 0)
       /\ UNCHANGED <<gOwner, gSubs, cleanEff>>
  ELSE UNCHANGED ghostV

Disconnected(c) == [RV EXCEPT !.conn = FALSE, !.seen = now, !.gdt = now, !.exp = ex[c]]

GRec(x, en, xp, o, sb, cl) == [ex |-> x, end |-> en, exp |-> xp, owner |-> o, subs |-> sb, clean |-> cl]

GNow  == GRec(gEx, gEnd, gExp, gOwner, gSubs, cleanEff)
GNext == GRec(gEx', gEnd', gExp', gOwner', gSubs', cleanEff')

EndG(g, c) == IF g.owner = c THEN [g EXCEPT !.end = now, !.ex = (g.ex /\ g.exp > 0)] ELSE g

Room == GC = "OFF" \/ Len(pbuf) < MaxPend

Entry == [r |-> IF cache'.ex THEN ToDisk(cache') ELSE NoDisk, g |-> GNext, f |-> FALSE, a |-> FALSE]

Log(c, w, ended) ==
  IF GC = "OFF" THEN UNCHANGED gcV
  ELSE LET base == IF ended
                   THEN [i \in DOMAIN pbuf |-> [pbuf[i] EXCEPT !.g = EndG(@, c)]]
                   ELSE pbuf
       IN /\ dg' = IF ended THEN EndG(dg, c) ELSE dg
          /\ pbuf' = IF w THEN Append(base, Entry) ELSE base
          /\ wpos' = IF w /\ c \in Conns THEN [wpos EXCEPT ![c] = Len(base) + 1] ELSE wpos
          /\ UNCHANGED <<dpos, ackSafe>>

AckReady(c) ==
  CASE GC \in {"OFF", "ACKEARLY"} -> TRUE
    [] GC \in {"PERWRITER", "PERWRITER_MONO"} -> IF wpos[c] = 0 THEN TRUE ELSE pbuf[wpos[c]].f
    [] OTHER                     -> wpos[c] = 0

MarkAck(c) ==
  IF GC = "OFF" THEN UNCHANGED gcV
  ELSE /\ pbuf' = IF wpos[c] = 0 THEN pbuf ELSE [pbuf EXCEPT ![wpos[c]].a = TRUE]
       /\ UNCHANGED <<dg, dpos, wpos, ackSafe>>

TypeOK ==
  /\ pc \in [Conns -> Pcs]
  /\ tok \in [Conns -> 0..MaxTok]
  /\ clr \in [Conns -> BOOLEAN]
  /\ ex \in [Conns -> {0, 1}]
  /\ loc \in [Conns -> SUBSET Subs]
  /\ pend \in [Conns -> Subs]
  /\ cborn \in [Conns -> 0..MaxTok]
  /\ startEff \in [Conns -> SUBSET (1..MaxTok)]
  /\ spOK \in [Conns -> BOOLEAN]
  /\ linOK \in [Conns -> BOOLEAN]
  /\ subOK \in [Conns -> BOOLEAN]
  /\ abortOK \in [Conns -> BOOLEAN]
  /\ failed \in [Conns -> BOOLEAN]
  /\ owner \in Conns \cup {None}
  /\ hwm \in 0..MaxTok
  /\ rsubs \in SUBSET Subs
  /\ rsPend \in 0..MaxTok
  /\ swPend \in BOOLEAN
  /\ nextId \in 1..MaxTok
  /\ cache.ex \in BOOLEAN /\ cache.subs \subseteq Subs /\ cache.tok \in 0..MaxTok
  /\ disk.ex \in BOOLEAN /\ disk.subs \subseteq Subs /\ disk.ts \in -1..MaxT
  /\ dirty \in BOOLEAN
  /\ now \in 0..MaxT /\ boots \in 0..MaxBoots /\ bootT \in 0..MaxT
  /\ gEx \in BOOLEAN /\ gEnd \in -1..MaxT /\ gExp \in {0, 1}
  /\ gOwner \in Conns \cup {None} /\ gSubs \subseteq Subs
  /\ cleanEff \subseteq 1..MaxTok
  /\ Len(pbuf) <= MaxPend
  /\ \A i \in DOMAIN pbuf : pbuf[i].f \in BOOLEAN /\ pbuf[i].a \in BOOLEAN
  /\ dpos \in 0..MaxPend
  /\ wpos \in [Conns -> 0..MaxPend]
  /\ ackSafe \in BOOLEAN

Init ==
  /\ pc = [c \in Conns |-> "idle"]
  /\ tok = [c \in Conns |-> 0]
  /\ clr = [c \in Conns |-> FALSE]
  /\ ex = [c \in Conns |-> 0]
  /\ loc = [c \in Conns |-> {}]
  /\ pend = [c \in Conns |-> CHOOSE s \in Subs : TRUE]
  /\ cborn = [c \in Conns |-> 0]
  /\ startEff = [c \in Conns |-> {}]
  /\ spOK = [c \in Conns |-> TRUE]
  /\ linOK = [c \in Conns |-> TRUE]
  /\ subOK = [c \in Conns |-> TRUE]
  /\ abortOK = [c \in Conns |-> TRUE]
  /\ failed = [c \in Conns |-> FALSE]
  /\ owner = None
  /\ hwm = 0
  /\ rsubs = {}
  /\ rsPend = 0
  /\ swPend = FALSE
  /\ nextId = 1
  /\ cache = NoRec
  /\ disk = NoDisk
  /\ dirty = FALSE
  /\ now = 0
  /\ boots = 0
  /\ bootT = 0
  /\ gEx = FALSE
  /\ gEnd = -1
  /\ gExp = 0
  /\ gOwner = None
  /\ gSubs = {}
  /\ cleanEff = {}
  /\ pbuf = <<>>
  /\ dg = GRec(FALSE, -1, 0, None, {}, {})
  /\ dpos = 0
  /\ wpos = [c \in Conns |-> 0]
  /\ ackSafe = TRUE

SpCheck(cl, resume) == (resume => GAlive) /\ ((~cl /\ GAlive) => resume)

LClaim(c, cl, e) ==
  /\ IsLock /\ On("claim") /\ pc[c] = "idle"
  /\ Room
  /\ LET r == ReadRes
         t == nextId
         resume == ~cl /\ r.ex /\ r.exp > 0
         rec == [ex |-> TRUE, tok |-> t, subs |-> IF resume THEN r.subs ELSE {}, exp |-> e,
                 conn |-> TRUE, seen |-> now, gdt |-> 0, born |-> IF resume THEN r.born ELSE t]
     IN /\ Wr(rec, On("wt_claim"), DiskAfterRead)
        /\ rsubs' = rec.subs
        /\ loc' = [loc EXCEPT ![c] = rec.subs]
        /\ cborn' = [cborn EXCEPT ![c] = rec.born]
        /\ clr' = [clr EXCEPT ![c] = ~resume]
        /\ spOK' = [spOK EXCEPT ![c] = SpCheck(cl, resume)]
        /\ linOK' = [linOK EXCEPT ![c] = resume => \A x \in cleanEff : r.born >= x]
        /\ subOK' = [subOK EXCEPT ![c] = resume => r.subs = gSubs]
        /\ gSubs' = rec.subs
        /\ cleanEff' = IF resume THEN cleanEff ELSE cleanEff \cup {t}
  /\ owner' = c
  /\ tok' = [tok EXCEPT ![c] = nextId]
  /\ nextId' = nextId + 1
  /\ ex' = [ex EXCEPT ![c] = e]
  /\ gEx' = TRUE /\ gEnd' = -1 /\ gExp' = e /\ gOwner' = c
  /\ pc' = [pc EXCEPT ![c] = "connack"]
  /\ UNCHANGED <<pend, startEff, abortOK, failed, hwm, rsPend, swPend, timeV>>
  /\ Log(c, TRUE, FALSE)

HSRead(c, cl, e) ==
  /\ Legacy("claim") /\ pc[c] = "idle"
  /\ LET r == ReadRes
         bound == r.ex /\ r.exp = 0 /\ owner # None
         resume == ~cl /\ ~bound /\ r.ex
     IN /\ clr' = [clr EXCEPT ![c] = ~resume]
        /\ loc' = [loc EXCEPT ![c] = IF resume THEN r.subs ELSE {}]
        /\ cborn' = [cborn EXCEPT ![c] = IF resume THEN r.born ELSE 0]
        /\ spOK' = [spOK EXCEPT ![c] = SpCheck(cl, resume)]
        /\ subOK' = [subOK EXCEPT ![c] = resume => r.subs = gSubs]
  /\ ExpireOnRead
  /\ ex' = [ex EXCEPT ![c] = e]
  /\ pc' = [pc EXCEPT ![c] = "store"]
  /\ UNCHANGED <<tok, pend, startEff, linOK, abortOK, failed, routerV, timeV, ghostV>>
  /\ UNCHANGED gcV

HSStore(c) ==
  /\ pc[c] = "store"
  /\ LET t == nextId
         b == IF cborn[c] = 0 THEN t ELSE cborn[c]
         rec == [ex |-> TRUE, tok |-> t, subs |-> loc[c], exp |-> ex[c], conn |-> TRUE,
                 seen |-> now, gdt |-> 0, born |-> b]
     IN /\ Wr(rec, On("wt_claim"), disk)
        /\ cborn' = [cborn EXCEPT ![c] = b]
        /\ linOK' = [linOK EXCEPT ![c] = cborn[c] # 0 => \A x \in cleanEff : cborn[c] >= x]
        /\ cleanEff' = IF cborn[c] = 0 THEN cleanEff \cup {t} ELSE cleanEff
  /\ tok' = [tok EXCEPT ![c] = nextId]
  /\ nextId' = nextId + 1
  /\ pc' = [pc EXCEPT ![c] = "hconnack"]
  /\ UNCHANGED <<clr, ex, loc, pend, startEff, spOK, subOK, abortOK, failed,
                 owner, hwm, rsubs, rsPend, swPend, timeV,
                 gEx, gEnd, gExp, gOwner, gSubs>>
  /\ UNCHANGED gcV

HConnackOK(c) ==
  /\ pc[c] = "hconnack"
  /\ pc' = [pc EXCEPT ![c] = "register"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 routerV, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

HConnackFail(c) ==
  /\ pc[c] = "hconnack"
  /\ failed' = [failed EXCEPT ![c] = TRUE]
  /\ pc' = [pc EXCEPT ![c] = "failrel"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK,
                 routerV, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

FailRel(c) ==
  /\ pc[c] = "failrel"
  /\ IF Owns(c)
     THEN Wr([View EXCEPT !.conn = FALSE, !.seen = now, !.gdt = now], On("wt_disc"), disk)
     ELSE UNCHANGED storeV
  /\ pc' = [pc EXCEPT ![c] = "done"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 routerV, timeV, ghostV>>
  /\ UNCHANGED gcV

Register(c) ==
  /\ pc[c] = "register"
  /\ owner' = c
  /\ rsubs' = IF clr[c] THEN {} ELSE rsubs
  /\ gEx' = TRUE /\ gEnd' = -1 /\ gExp' = ex[c] /\ gOwner' = c /\ gSubs' = loc[c]
  /\ pc' = [pc EXCEPT ![c] = IF clr[c] THEN "live" ELSE "install"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 hwm, rsPend, swPend, nextId, storeV, timeV, cleanEff>>
  /\ UNCHANGED gcV

Install(c) ==
  /\ pc[c] = "install"
  /\ rsubs' = rsubs \cup loc[c]
  /\ pc' = [pc EXCEPT ![c] = "live"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 owner, hwm, rsPend, swPend, nextId, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

CasRec(c, r, t) ==
  LET resume == ~clr[c] /\ r.ex /\ r.exp > 0
  IN [resume |-> resume,
      rec |-> [ex |-> TRUE, tok |-> t, subs |-> IF resume THEN r.subs ELSE {}, exp |-> ex[c],
               conn |-> TRUE, seen |-> now, gdt |-> 0, born |-> IF resume THEN r.born ELSE t]]

SStore(c, cl, e) ==
  /\ MECH = "CAS" /\ pc[c] = "idle"
  /\ LET r == ReadRes
         t == nextId
         resume == ~cl /\ r.ex /\ r.exp > 0
         rec == [ex |-> TRUE, tok |-> t, subs |-> IF resume THEN r.subs ELSE {}, exp |-> e,
                 conn |-> TRUE, seen |-> now, gdt |-> 0, born |-> IF resume THEN r.born ELSE t]
     IN /\ Wr(rec, On("wt_claim"), DiskAfterRead)
        /\ loc' = [loc EXCEPT ![c] = rec.subs]
        /\ clr' = [clr EXCEPT ![c] = ~resume]
        /\ spOK' = [spOK EXCEPT ![c] = SpCheck(cl, resume)]
        /\ linOK' = [linOK EXCEPT ![c] = resume => \A x \in cleanEff : r.born >= x]
        /\ subOK' = [subOK EXCEPT ![c] = resume => r.subs = gSubs]
        /\ gSubs' = rec.subs
        /\ cleanEff' = IF resume THEN cleanEff ELSE cleanEff \cup {t}
  /\ tok' = [tok EXCEPT ![c] = nextId]
  /\ nextId' = nextId + 1
  /\ ex' = [ex EXCEPT ![c] = e]
  /\ gEx' = TRUE /\ gEnd' = -1 /\ gExp' = e /\ gOwner' = c
  /\ pc' = [pc EXCEPT ![c] = "creg"]
  /\ UNCHANGED <<pend, cborn, startEff, abortOK, failed, owner, hwm, rsubs, rsPend, swPend, timeV>>
  /\ UNCHANGED gcV

NewerActive(c) == \E d \in Conns : tok[d] > tok[c]

SReg(c) ==
  /\ pc[c] = "creg"
  /\ IF tok[c] > hwm
     THEN /\ owner' = c /\ hwm' = tok[c] /\ rsubs' = loc[c]
          /\ pc' = [pc EXCEPT ![c] = "connack"]
          /\ UNCHANGED <<failed, abortOK>>
     ELSE /\ pc' = [pc EXCEPT ![c] = "release"]
          /\ failed' = [failed EXCEPT ![c] = TRUE]
          /\ abortOK' = [abortOK EXCEPT ![c] = NewerActive(c)]
          /\ UNCHANGED <<owner, hwm, rsubs>>
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK,
                 rsPend, swPend, nextId, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

RReg(c, cl, e) ==
  /\ MECH = "CASSPLIT" /\ pc[c] = "idle"
  /\ owner' = c
  /\ tok' = [tok EXCEPT ![c] = nextId]
  /\ nextId' = nextId + 1
  /\ clr' = [clr EXCEPT ![c] = cl]
  /\ ex' = [ex EXCEPT ![c] = e]
  /\ startEff' = [startEff EXCEPT ![c] = cleanEff]
  /\ IF cl
     THEN rsubs' = {} /\ cleanEff' = cleanEff \cup {nextId}
     ELSE UNCHANGED <<rsubs, cleanEff>>
  /\ pc' = [pc EXCEPT ![c] = "cstore"]
  /\ UNCHANGED <<loc, pend, cborn, spOK, linOK, subOK, abortOK, failed, hwm, rsPend, swPend,
                 storeV, timeV, gEx, gEnd, gExp, gOwner, gSubs>>
  /\ UNCHANGED gcV

RStore(c) ==
  /\ pc[c] = "cstore"
  /\ LET r == ReadRes
         cr == CasRec(c, r, tok[c])
     IN IF r.ex /\ r.tok > tok[c]
        THEN /\ pc' = [pc EXCEPT ![c] = "release"]
             /\ failed' = [failed EXCEPT ![c] = TRUE]
             /\ abortOK' = [abortOK EXCEPT ![c] = NewerActive(c)]
             /\ UNCHANGED <<loc, spOK, linOK, subOK, storeV, ghostV>>
        ELSE /\ Wr(cr.rec, On("wt_claim"), DiskAfterRead)
             /\ loc' = [loc EXCEPT ![c] = cr.rec.subs]
             /\ spOK' = [spOK EXCEPT ![c] = SpCheck(clr[c], cr.resume)]
             /\ linOK' = [linOK EXCEPT ![c] = cr.resume => \A x \in startEff[c] : r.born >= x]
             /\ subOK' = [subOK EXCEPT ![c] = cr.resume => r.subs = gSubs]
             /\ gEx' = TRUE /\ gEnd' = -1 /\ gExp' = ex[c] /\ gOwner' = c /\ gSubs' = cr.rec.subs
             /\ cleanEff' = IF cr.resume THEN cleanEff ELSE cleanEff \cup {tok[c]}
             /\ pc' = [pc EXCEPT ![c] = "cinstall"]
             /\ UNCHANGED <<failed, abortOK>>
  /\ UNCHANGED <<tok, clr, ex, pend, cborn, startEff, routerV, timeV>>
  /\ UNCHANGED gcV

RInstall(c) ==
  /\ pc[c] = "cinstall"
  /\ rsubs' = IF owner = c THEN loc[c] ELSE rsubs
  /\ pc' = [pc EXCEPT ![c] = "connack"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 owner, hwm, rsPend, swPend, nextId, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

ConnackOK(c) ==
  /\ pc[c] = "connack"
  /\ AckReady(c)
  /\ pc' = [pc EXCEPT ![c] = "live"]
  /\ MarkAck(c)
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 routerV, storeV, timeV, ghostV>>

ConnackFail(c) ==
  /\ pc[c] = "connack"
  /\ failed' = [failed EXCEPT ![c] = TRUE]
  /\ GhostEnd(c, ex[c])
  /\ pc' = [pc EXCEPT ![c] = "release"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK,
                 routerV, storeV, timeV>>
  /\ Log(c, FALSE, TRUE)

Toggle(set, s, add) == IF add THEN set \cup {s} ELSE set \ {s}

LSub(c, s, add) ==
  /\ IsLock /\ On("sub") /\ pc[c] = "live" /\ owner = c
  /\ (add => s \notin loc[c]) /\ (~add => s \in loc[c])
  /\ rsubs' = Toggle(rsubs, s, add)
  /\ loc' = [loc EXCEPT ![c] = Toggle(loc[c], s, add)]
  /\ Owns(c) => Room
  /\ IF Owns(c)
     THEN /\ Wr([RV EXCEPT !.subs = Toggle(RV.subs, s, add)], On("wt_sub"), disk)
          /\ gSubs' = IF gOwner = c THEN Toggle(gSubs, s, add) ELSE gSubs
     ELSE UNCHANGED <<storeV, gSubs>>
  /\ pc' = IF GC = "OFF" THEN pc ELSE [pc EXCEPT ![c] = "suback"]
  /\ UNCHANGED <<tok, clr, ex, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 owner, hwm, rsPend, swPend, nextId, timeV, gEx, gEnd, gExp, gOwner, cleanEff>>
  /\ Log(c, Owns(c), FALSE)

SubAck(c) ==
  /\ pc[c] = "suback"
  /\ AckReady(c)
  /\ pc' = [pc EXCEPT ![c] = "live"]
  /\ MarkAck(c)
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 routerV, storeV, timeV, ghostV>>

SubRouter(c, s, add) ==
  /\ (IsCas \/ Legacy("sub")) /\ pc[c] = "live" /\ owner = c
  /\ (add => s \notin loc[c]) /\ (~add => s \in loc[c])
  /\ rsubs' = Toggle(rsubs, s, add)
  /\ pend' = [pend EXCEPT ![c] = s]
  /\ pc' = [pc EXCEPT ![c] = IF add THEN "substore" ELSE "unsubstore"]
  /\ UNCHANGED <<tok, clr, ex, loc, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 owner, hwm, rsPend, swPend, nextId, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

SubStore(c) ==
  /\ pc[c] \in {"substore", "unsubstore"}
  /\ LET add == pc[c] = "substore"
         nl == Toggle(loc[c], pend[c], add)
     IN /\ loc' = [loc EXCEPT ![c] = nl]
        /\ IF IsCas
           THEN IF Owns(c)
                THEN /\ Wr([View EXCEPT !.subs = Toggle(View.subs, pend[c], add)], On("wt_sub"), disk)
                     /\ gSubs' = IF gOwner = c THEN Toggle(gSubs, pend[c], add) ELSE gSubs
                ELSE UNCHANGED <<storeV, gSubs>>
           ELSE /\ Wr([ex |-> TRUE, tok |-> tok[c], subs |-> nl, exp |-> ex[c], conn |-> TRUE,
                       seen |-> now, gdt |-> 0, born |-> cborn[c]], On("wt_sub"), disk)
                /\ gSubs' = IF gOwner = c THEN Toggle(gSubs, pend[c], add) ELSE gSubs
  /\ pc' = [pc EXCEPT ![c] = "live"]
  /\ UNCHANGED <<tok, clr, ex, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 routerV, timeV, gEx, gEnd, gExp, gOwner, cleanEff>>
  /\ UNCHANGED gcV

Close(c, e2) ==
  /\ pc[c] = "live"
  /\ e2 \in IF ex[c] = 0 THEN {0} ELSE {0, ex[c]}
  /\ LET w == On("disc") /\ e2 # ex[c] /\ Owns(c)
     IN /\ w => Room
        /\ ex' = [ex EXCEPT ![c] = e2]
        /\ GhostEnd(c, e2)
        /\ IF w THEN Wr([RV EXCEPT !.exp = e2], On("wt_disc"), disk) ELSE UNCHANGED storeV
        /\ pc' = [pc EXCEPT ![c] = "release"]
        /\ UNCHANGED <<tok, clr, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                       routerV, timeV>>
        /\ Log(c, w, TRUE)

LRelease(c) ==
  /\ IsLock /\ On("release") /\ pc[c] = "release"
  /\ (owner = c /\ Owns(c)) => Room
  /\ IF owner = c
     THEN /\ owner' = None
          /\ rsubs' = IF ex[c] = 0 THEN {} ELSE rsubs
          /\ IF Owns(c)
             THEN IF ex[c] = 0 THEN Remove(On("wt_remove"))
                  ELSE Wr(Disconnected(c), On("wt_disc"), disk)
             ELSE UNCHANGED storeV
     ELSE UNCHANGED <<owner, rsubs, storeV>>
  /\ pc' = [pc EXCEPT ![c] = IF GC = "OFF" THEN "done" ELSE "relwait"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 hwm, rsPend, swPend, nextId, timeV, ghostV>>
  /\ Log(c, owner = c /\ Owns(c), FALSE)

RelDone(c) ==
  /\ pc[c] = "relwait"
  /\ AckReady(c)
  /\ pc' = [pc EXCEPT ![c] = "done"]
  /\ MarkAck(c)
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 routerV, storeV, timeV, ghostV>>

Release(c) ==
  /\ Legacy("release") /\ pc[c] = "release"
  /\ IF owner = c
     THEN /\ owner' = None
          /\ pc' = [pc EXCEPT ![c] = IF ex[c] = 0 THEN "strip" ELSE "cleanup"]
     ELSE /\ UNCHANGED owner
          /\ pc' = [pc EXCEPT ![c] = "done"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 hwm, rsubs, rsPend, swPend, nextId, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

Strip(c) ==
  /\ pc[c] = "strip"
  /\ rsubs' = {}
  /\ pc' = [pc EXCEPT ![c] = "cleanup"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 owner, hwm, rsPend, swPend, nextId, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

Cleanup(c) ==
  /\ pc[c] = "cleanup"
  /\ IF Owns(c) THEN Wr(Disconnected(c), On("wt_disc"), disk) ELSE UNCHANGED storeV
  /\ pc' = [pc EXCEPT ![c] = IF ex[c] = 0 THEN "rmowned" ELSE "done"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 routerV, timeV, ghostV>>
  /\ UNCHANGED gcV

RmOwned(c) ==
  /\ pc[c] = "rmowned"
  /\ IF Owns(c) THEN Remove(On("wt_remove")) ELSE UNCHANGED storeV
  /\ pc' = [pc EXCEPT ![c] = "done"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 routerV, timeV, ghostV>>
  /\ UNCHANGED gcV

CRelease(c) ==
  /\ IsCas /\ pc[c] = "release"
  /\ IF owner = c
     THEN /\ owner' = None
          /\ rsubs' = IF ex[c] = 0 THEN {} ELSE rsubs
     ELSE UNCHANGED <<owner, rsubs>>
  /\ pc' = [pc EXCEPT ![c] = "cclean"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 hwm, rsPend, swPend, nextId, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

CClean(c) ==
  /\ pc[c] = "cclean"
  /\ IF Owns(c)
     THEN IF ex[c] = 0 THEN Remove(On("wt_remove"))
          ELSE Wr(Disconnected(c), On("wt_disc"), disk)
     ELSE UNCHANGED storeV
  /\ pc' = [pc EXCEPT ![c] = "done"]
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 routerV, timeV, ghostV>>
  /\ UNCHANGED gcV

LSweep ==
  /\ IsLock /\ On("sweep")
  /\ owner = None
  /\ \/ RV.ex /\ Expired(RV)
     \/ ~RV.ex /\ rsubs # {}
  /\ Room
  /\ Remove(On("wt_remove"))
  /\ rsubs' = {}
  /\ UNCHANGED <<connV, owner, hwm, rsPend, swPend, nextId, timeV, ghostV>>
  /\ Log(None, TRUE, FALSE)

FileSweepCond == disk.ex /\ IF cache.ex THEN Expired(cache) ELSE Expired(Load(disk))

FileSweepRead ==
  /\ Legacy("sweep")
  /\ ~swPend /\ FileSweepCond
  /\ swPend' = TRUE
  /\ UNCHANGED <<connV, owner, hwm, rsubs, rsPend, nextId, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

FileSweepApply ==
  /\ swPend
  /\ cache' = NoRec /\ disk' = NoDisk /\ dirty' = FALSE
  /\ swPend' = FALSE
  /\ UNCHANGED <<connV, owner, hwm, rsubs, rsPend, nextId, timeV, ghostV>>
  /\ UNCHANGED gcV

CFileSweep ==
  /\ IsCas
  /\ View.ex /\ Expired(View)
  /\ Remove(On("wt_remove"))
  /\ UNCHANGED <<connV, routerV, timeV, ghostV>>
  /\ UNCHANGED gcV

RouterSweepDecide ==
  /\ IsCas \/ Legacy("sweep")
  /\ owner = None /\ rsubs # {} /\ rsPend = 0
  /\ ~ReadRes.ex
  /\ ExpireOnRead
  /\ rsPend' = nextId
  /\ UNCHANGED <<connV, owner, hwm, rsubs, swPend, nextId, timeV, ghostV>>
  /\ UNCHANGED gcV

RouterSweepApply ==
  /\ rsPend > 0
  /\ rsubs' = IF ~IsCas \/ (owner = None /\ nextId = rsPend) THEN {} ELSE rsubs
  /\ rsPend' = 0
  /\ UNCHANGED <<connV, owner, hwm, swPend, nextId, storeV, timeV, ghostV>>
  /\ UNCHANGED gcV

Flush ==
  /\ GC = "OFF" /\ dirty
  /\ disk' = IF cache.ex THEN ToDisk(cache) ELSE NoDisk
  /\ dirty' = FALSE
  /\ UNCHANGED <<connV, routerV, cache, timeV, ghostV>>
  /\ UNCHANGED gcV

GFlushPrefix(k) ==
  /\ GC \in {"WHOLE", "PREFIX", "ACKEARLY", "NORYW"}
  /\ k \in 1..Len(pbuf)
  /\ GC = "WHOLE" => k = Len(pbuf)
  /\ disk' = pbuf[k].r
  /\ dg' = pbuf[k].g
  /\ pbuf' = SubSeq(pbuf, k + 1, Len(pbuf))
  /\ dirty' = (k < Len(pbuf))
  /\ wpos' = [c \in Conns |-> IF wpos[c] <= k THEN 0 ELSE wpos[c] - k]
  /\ UNCHANGED <<dpos, ackSafe, connV, routerV, cache, timeV, ghostV>>

FlushedPrefix(q, lim) ==
  CHOOSE j \in 0..lim : (\A l \in 1..j : q[l].f) /\ (j = lim \/ ~q[j + 1].f)

GFlushOne(i) ==
  /\ GC \in {"PERWRITER", "PERWRITER_MONO"}
  /\ i \in 1..Len(pbuf) /\ ~pbuf[i].f
  /\ LET q == [pbuf EXCEPT ![i].f = TRUE]
         m == IF GC = "PERWRITER_MONO" THEN i ELSE FlushedPrefix(q, i)
     IN /\ disk' = pbuf[i].r
        /\ dg' = pbuf[i].g
        /\ pbuf' = SubSeq(q, m + 1, Len(q))
        /\ dpos' = i - m
        /\ dirty' = \E l \in (m + 1)..Len(q) : ~q[l].f
        /\ wpos' = [c \in Conns |-> IF wpos[c] <= m THEN 0 ELSE wpos[c] - m]
  /\ UNCHANGED <<ackSafe, connV, routerV, cache, timeV, ghostV>>

GFlush == (\E k \in 1..Len(pbuf) : GFlushPrefix(k)) \/ (\E i \in 1..Len(pbuf) : GFlushOne(i))

Persist == Flush \/ GFlush

TrueEnd ==
  IF GC # "OFF" THEN (IF disk.ts = -1 THEN now ELSE disk.gdt)
  ELSE IF cache.ex THEN (IF cache.conn THEN now ELSE cache.gdt) ELSE disk.gdt

AckedLost == {i \in DOMAIN pbuf : pbuf[i].a /\ i > dpos}

CrashBase ==
  IF GC = "OFF" THEN GNow
  ELSE IF AckedLost = {} THEN dg
  ELSE pbuf[CHOOSE i \in AckedLost : \A j \in AckedLost : j <= i].g

Restart ==
  /\ boots < MaxBoots
  /\ boots' = boots + 1
  /\ bootT' = now
  /\ pc' = [c \in Conns |-> IF pc[c] \in {"idle", "done"} THEN pc[c] ELSE "dead"]
  /\ owner' = None
  /\ hwm' = 0
  /\ rsubs' = IF On("rebuild") /\ disk.ex /\ disk.exp > 0 THEN disk.subs ELSE {}
  /\ rsPend' = 0
  /\ swPend' = FALSE
  /\ cache' = NoRec
  /\ dirty' = FALSE
  /\ disk' = IF disk.ex
             THEN [disk EXCEPT !.gdt = TrueEnd,
                               !.ts = IF On("stamp") /\ disk.ts = -1 THEN now ELSE disk.ts]
             ELSE disk
  /\ LET b == CrashBase
     IN /\ gEnd' = IF b.ex /\ b.end = -1 THEN now ELSE b.end
        /\ gEx' = (b.ex /\ b.exp > 0)
        /\ gExp' = b.exp
        /\ gSubs' = b.subs
        /\ cleanEff' = b.clean
  /\ gOwner' = None
  /\ UNCHANGED <<tok, clr, ex, loc, pend, cborn, startEff, spOK, linOK, subOK, abortOK, failed,
                 nextId, now>>
  /\ IF GC = "OFF"
     THEN UNCHANGED gcV
     ELSE /\ pbuf' = <<>>
          /\ dg' = GNext
          /\ dpos' = 0
          /\ wpos' = [c \in Conns |-> 0]
          /\ ackSafe' = (ackSafe /\ AckedLost = {})

Tick ==
  /\ now < MaxT
  /\ \A c \in Conns : pc[c] \notin Closing
  /\ \A i \in DOMAIN pbuf : pbuf[i].f
  /\ now' = now + 1
  /\ UNCHANGED <<connV, routerV, storeV, boots, bootT, ghostV>>
  /\ UNCHANGED gcV

ReleaseStep(c) ==
  \/ LRelease(c) \/ Release(c) \/ Strip(c) \/ Cleanup(c) \/ RmOwned(c) \/ FailRel(c)
  \/ CRelease(c) \/ CClean(c)

Handler(c) ==
  \/ HSStore(c) \/ HConnackOK(c) \/ Register(c) \/ Install(c)
  \/ SReg(c) \/ RStore(c) \/ RInstall(c) \/ ConnackOK(c) \/ SubStore(c)
  \/ SubAck(c) \/ RelDone(c)
  \/ ReleaseStep(c)

Sweeps ==
  \/ LSweep \/ FileSweepRead \/ FileSweepApply \/ CFileSweep
  \/ RouterSweepDecide \/ RouterSweepApply

Next ==
  \/ \E c \in Conns, cl \in BOOLEAN, e \in {0, 1} :
       LClaim(c, cl, e) \/ HSRead(c, cl, e) \/ SStore(c, cl, e) \/ RReg(c, cl, e)
  \/ \E c \in Conns : Handler(c) \/ HConnackFail(c) \/ ConnackFail(c)
  \/ \E c \in Conns, s \in Subs, add \in BOOLEAN : LSub(c, s, add) \/ SubRouter(c, s, add)
  \/ \E c \in Conns, e2 \in {0, 1} : Close(c, e2)
  \/ Sweeps \/ Persist \/ Restart \/ Tick

SysNext == (\E c \in Conns : Handler(c)) \/ Sweeps \/ Persist

Spec == Init /\ [][Next]_vars
          /\ WF_vars(\E c \in Conns : Handler(c)) /\ WF_vars(Sweeps)
          /\ WF_vars(Persist) /\ WF_vars(Tick)

SpecNoSweepFair == Init /\ [][Next]_vars
          /\ WF_vars(\E c \in Conns : Handler(c))
          /\ WF_vars(Persist) /\ WF_vars(Tick)

Sym == Permutations(Conns)

InvOwnershipAgree ==
  \A c, d \in Conns :
    (pc[c] \notin Ended /\ pc[d] \notin Ended /\ owner = c /\ OwnsNow(d)) => c = d

InvNoLeakedConnected == (View.ex /\ View.conn) => Holder(View.tok)

InvLiveOwnerSessionKept ==
  \A c \in Conns :
    (owner = c /\ pc[c] \in LivePC) => (View.ex /\ View.conn /\ View.tok >= tok[c])

InvFailedHandshakeHarmless ==
  \A c, d \in Conns :
    (failed[c] /\ pc[c] = "done" /\ owner = d /\ pc[d] \in LivePC)
      => (View.ex /\ View.conn /\ View.tok # tok[c])

InvSessionPresent == \A c \in Conns : spOK[c]

InvNoResurrection == \A c \in Conns : linOK[c]

InvAckedSubsDurable == \A c \in Conns : subOK[c]

InvAbortJustified == \A c \in Conns : abortOK[c]

InvCleanStart ==
  \A c \in Conns :
    (owner = c /\ pc[c] = "live" /\ clr[c])
      => (rsubs \subseteq loc[c] /\ ((View.ex /\ View.tok = tok[c]) => View.subs \subseteq loc[c]))

InvRouterMirrors ==
  rsubs = IF owner # None THEN loc[owner]
          ELSE IF View.ex /\ View.exp > 0 THEN View.subs ELSE {}

Quiet == /\ \A c \in Conns : pc[c] \in Ended \cup {"live"}
         /\ rsPend = 0 /\ ~swPend

InvQuiescentConsistent ==
  \A c \in Conns :
    (Quiet /\ owner = c /\ pc[c] = "live")
      => (rsubs = loc[c] /\ View.ex /\ View.subs = loc[c])

InvExpiryExact == (View.ex /\ ~View.conn) => View.seen = View.gdt

StaleSession ==
  /\ View.ex
  /\ ~Holder(View.tok)
  /\ (View.conn \/ now > View.gdt + Dur(View.exp))

StaleRoutes == owner = None /\ ~View.ex /\ rsubs # {}

Stale == StaleSession \/ StaleRoutes

InvProgress == (now = MaxT /\ ~ENABLED SysNext) => ~Stale

InvAckedStateSurvivesCrash == ackSafe

InvPendNotFull == Len(pbuf) < MaxPend

Acked(c) == ~failed[c] /\ pc[c] \in LivePC \cup {"release", "relwait", "done"}

InvAckedClaimCorrect == \A c \in Conns : Acked(c) => (spOK[c] /\ linOK[c] /\ subOK[c])

EventuallyClean == []<>(now < MaxT \/ ~Stale)

NegTimeNeverEnds == []<>(now < MaxT)

NegNoResumeAfterRestart ==
  ~(\E c, d \in Conns : pc[c] = "connack" /\ ~clr[c] /\ loc[c] # {}
                        /\ pc[d] = "dead" /\ tok[d] = cborn[c])

NegNoResumingTakeover ==
  ~(\E c, d \in Conns : c # d /\ pc[c] = "live" /\ pc[d] = "connack" /\ owner = d /\ ~clr[d]
                        /\ loc[d] # {})

=============================================================================
