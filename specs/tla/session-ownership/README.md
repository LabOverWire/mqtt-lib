# Session ownership: TLA+ model

Model of how the `mqtt5` broker decides who owns a ClientID's session, and how it keeps the
router (live routes), the session store (cache plus file) and the MQTT-visible session state
(Session Present, Clean Start, Session Expiry, acknowledged subscriptions) in agreement while
connections race, handshakes fail, sweeps run and the broker crashes.

It consolidates three independent models that reached the same verdict (see "Provenance"). The
chosen design is `MECH = "LOCK"`. Today's code is kept as `MECH = "CURRENT"`, the rejected
lock-free alternatives as `MECH = "CAS"` and `MECH = "CASSPLIT"`, and every element of the chosen
design can be switched off individually with `DROP` so that the spec shows why it is needed.

## Files

- `SessionOwnership.tla`: the spec. One module contains all four mechanisms.
- `SessionOwnership.cfg`: the chosen design, main safety run (3 connections, 2 filters, 1 restart).
- `SessionOwnership_boots2.cfg`: the chosen design with 2 restarts.
- `SessionOwnership_live.cfg`: liveness of the chosen design.
- `SessionOwnership_NEG_*.cfg`: negative controls. Each must fail.
- `SessionOwnership_ABL_<fix>.cfg`: the chosen design with one element dropped. Each must fail.
  `_ABL_<fix>_core.cfg` repeats an ablation without the property that fails first, to show the
  deeper failure behind it. `_ABL_wt_claim_resurrect.cfg` checks only `InvNoResurrection`.
- `SessionOwnership_CUR_<Property>.cfg`: today's code, one cfg per property it breaks.
- `SessionOwnership_CAS_*.cfg`, `SessionOwnership_CASSPLIT_*.cfg`: the rejected lock-free designs.
- `SessionOwnership_GC_*.cfg`: the chosen design with group commit (`GC` other than `"OFF"`), see
  "Group commit". `_GC_whole`, `_GC_prefix`, `_GC_prefix_boots2`, `_GC_prefix_live` and
  `_GC_perwriter_mono` must pass; `_GC_ABL_*` are the group-commit ablations and must fail.

Every cfg sets `GC` and `MaxPend`. All cfgs other than `_GC_*` use `GC = "OFF"` (every durable
write reaches the file in the step that makes it, as before group commit), and `MaxPend` is then
unused.

## The chosen design

All session state for one ClientID is guarded by one per-ClientID lock (the session slot). Every
step below runs as one critical section under that lock, so no other step for the same ClientID
can interleave with it.

1. **Claim before CONNACK** (`LClaim`). In one critical section the new connection:
   - reads the stored session (an expired one counts as absent and is removed),
   - decides resume or discard: resume only if Clean Start = 0 and a live session with
     Session Expiry > 0 exists; otherwise start empty [MQTT-3.1.2-4],
   - writes the new session record durably, with a fresh connection token,
   - registers itself as the router owner (displacing any previous owner),
   - sets the router's routes for the ClientID to exactly the claimed session's subscriptions
     (not a union with whatever the router held).

   CONNACK is sent only after the claim. Session Present is the claim's resume decision.
   The model takes `LClaim` as one atomic step, so the order of these effects inside the
   critical section is invisible to it: no other step for the ClientID can observe the state
   between them. The code writes before it registers so that a claim whose write fails changes
   nothing and the live owner stays registered; write failures are not modelled (see "Not
   modelled") and are covered by tests.
2. **Owner-only SUBSCRIBE / UNSUBSCRIBE** (`LSub`). Under the lock, and only if this connection
   is still the owner, the router routes and the stored subscriptions change together, and the
   store write is durable before SUBACK / UNSUBACK. The model changes one filter per step. The
   code applies every filter of one packet in one critical section and stores them in one
   write; that is a run of consecutive `LSub` steps that nothing can interleave with (the lock
   is held throughout), whose writes are merged into one record because a write replaces the
   whole record, and whose acknowledgement waits for that single write.
3. **Owner-only DISCONNECT expiry update, written at the DISCONNECT** (`Close` with `disc`).
   When the DISCONNECT packet is handled, in its own critical section under the lock and before
   the release step, a DISCONNECT that changes Session Expiry writes the new value to storage if
   the connection still owns the stored session. The release (step 4) then writes the disconnect
   time or removes the session. Deferring the new expiry to the release is `DROP = {"disc"}` and
   fails (`_ABL_disc`).
4. **Owner-only release, strip under the lock** (`LRelease`). When a connection ends it releases
   ownership only if it is still the owner. In the same critical section it either removes the
   session and strips its routes (expiry 0) or marks the session disconnected with the
   disconnect time (expiry > 0). A displaced connection's release touches nothing.
5. **Sweeps only without an owner, atomic re-check and remove** (`LSweep`). A sweep takes the
   lock, re-checks that there is no owner and that the session is expired (or absent while
   routes remain), and removes the session and its routes in the same critical section.
6. **Persisted disconnect timestamp, boot stamping, router rebuild** (`stamp`, `rebuild`,
   `Restart`). The disconnect time is stored with the session. At startup any session still
   marked connected is stamped as disconnected at boot time, and the router's routes are
   rebuilt from the stored sessions that have Session Expiry > 0.
7. **Every session write is durable** (`wt_claim`, `wt_sub`, `wt_disc`, `wt_remove`): the claim
   (including the clean-start discard, which replaces the record), subscription changes, the
   disconnect/expiry update and every removal (expiry-0 release, clean-start discard, sweeps).
   With `GC = "OFF"` each write is durable in the step that makes it. With group commit the
   same writes go through a pending buffer and every acknowledgement waits for the flush (see
   "Group commit").

### Why a lock and not compare-and-swap

The lock-free alternatives keep the router and the store as two separately updated objects and
fence stale writers with connection tokens:

- `CAS`: the hardened version the modelers converged on. The storage claim allocates a token,
  the router registration is fenced by a high-water mark, releases and sweeps are token- and
  generation-fenced, and a displaced connection cleans up only what it still owns. With all of
  that it satisfies every property except two: the router owner and the storage owner disagree
  between the two steps of a claim (`InvOwnershipAgree`), and the router is not an exact mirror
  of the session at every instant (`InvRouterMirrors`). Every consumer that reads router and
  store together would have to tolerate that window.
- `CASSPLIT`: register at the router first (Clean Start strips the routes there), then claim
  in storage with a token compare. A Clean Start that registered earlier can be undone by a
  later connection's storage claim that resumes the pre-clean-start session: the discarded
  session comes back (`InvNoResurrection`). This is the resurrection counterexample (see below).

The lock gives the stronger properties with fewer moving parts: no tombstones, no router
high-water mark, no generation-fenced sweeps.

## Model

- **One ClientID.** `Conns` are successive connection attempts for the same ClientID. Each
  attempt goes `idle`, claim, `connack`, `live`, `release`, `done`. A crash turns every attempt
  in progress into `dead`. An attempt that never started stays `idle` and may start after the
  crash.
- **Router.** `owner` (the registered connection) and `rsubs` (routes for the ClientID).
- **Session store.** `cache` (the in-memory record), `disk` (the file) and `dirty` (the cache
  holds a write, or a removal, not yet on disk). `View` is what a reader sees: the cache when it
  holds a record or a pending write, otherwise the file. A write is either write-through
  (durable at once) or write-behind (flushed later by `Flush`). A record carries the connection
  token `tok`, the subscriptions, the Session Expiry (`exp`: 0 or 1, meaning 0 or `E` ticks),
  whether it is connected, `seen` (the disconnect time the expiry counts from), and two ghost
  fields: `gdt` (the true disconnect time) and `born` (which fresh start created the session).
- **Time and crashes.** `now` advances to `MaxT`. `Restart` (up to `MaxBoots` times) loses the
  cache, the router and every connection in progress; the file survives.
- **Ghost state.** `gEx`, `gEnd`, `gExp`, `gOwner`, `gSubs` track the logical session as MQTT
  defines it: whether it exists, when it ended, its expiry, and the subscriptions acknowledged
  to its current owner. `cleanEff` is the set of `born` values of every fresh start that took
  effect. Per connection, `spOK`, `linOK`, `subOK` and `abortOK` record whether the claim's
  decisions were correct.
- **Mechanisms.** `MECH` selects the mechanism and `DROP` removes elements of the chosen
  design. Dropping an element reverts that element to today's behaviour:

| Element (`DROP` name) | Chosen design | Behaviour when dropped (today's code) |
|---|---|---|
| `claim` | `LClaim`: one critical section before CONNACK | `HSRead`, `HSStore`, CONNACK, `Register`, `Install`: read, then write, then CONNACK, then router register, then route union, all separate |
| `sub` | `LSub`: per filter, owner-only, router and store together | `SubRouter` then `SubStore`: router step, then a whole-record store overwrite with the connection's own token, not owner-checked |
| `disc` | `Close` writes a changed Session Expiry owner-only | the new expiry reaches the store only at release |
| `release` | `LRelease`: owner-only, strip and store update together | `Release`, `Strip`, `Cleanup`, `RmOwned`: router release, route strip, store update and store removal as separate steps |
| `sweep` | `LSweep`: owner-less, re-check and remove together | `FileSweepRead`/`FileSweepApply` and `RouterSweepDecide`/`RouterSweepApply`: decide, then apply without re-checking |
| `stamp` | disconnect time persisted, connected records stamped at boot | the file has no disconnect time; a loaded record's expiry counts from the time it is read |
| `rebuild` | router routes rebuilt from the file at restart | router starts empty after restart |
| `wt_claim`, `wt_sub`, `wt_disc` | durable writes | write-behind (`store_session` caches and marks dirty) |
| `wt_remove` | durable removal | write-behind removal (a pending removal is lost in a crash) |

`MECH = "CURRENT"` is every element dropped except `wt_remove`, because today's
`remove_session` / `remove_owned_session` already delete the file at once.

## Properties in plain words

All invariants below are checked in every run of the chosen design.

| Property | Meaning |
|---|---|
| `TypeOK` | Variables stay within their bounded types. |
| `InvOwnershipAgree` | The router owner and the connection whose token the stored session carries are the same connection whenever both are running. |
| `InvNoLeakedConnected` | A stored session marked connected always belongs to a running connection. No session stays "connected" after its connection is gone. |
| `InvLiveOwnerSessionKept` | While a connection is the live owner, the stored session exists, is marked connected and carries its token (or a newer one). Nothing removes or disconnects the live owner's session under it. |
| `InvFailedHandshakeHarmless` | A connection whose handshake failed leaves the current live owner's session intact and never leaves its own token in it. |
| `InvSessionPresent` | Session Present is correct against the logical session: SP = 1 only if a session with expiry > 0 is alive (not ended, or ended less than its expiry ago), and SP = 0 on Clean Start = 0 only if no such session is alive. (SP = 0 on Clean Start = 1 holds by construction: no claim resumes when Clean Start = 1.) |
| `InvNoResurrection` | A resumed session was not discarded or superseded by any fresh start that took effect before the resume. This covers Clean Start = 1 discards [MQTT-3.1.2-4] and holds across crashes. It is the linearizability check for claims. |
| `InvAckedSubsDurable` | A resumed session carries exactly the subscriptions acknowledged (SUBACK / UNSUBACK) to the previous owner, including across a crash. |
| `InvAbortJustified` | A claim that aborts does so only because a newer claim exists (only the lock-free mechanisms can abort a claim; trivially true for LOCK and CURRENT). |
| `InvCleanStart` | A live owner that started empty has no routes and no stored subscriptions other than the ones it subscribed itself [MQTT-3.1.2-4]. |
| `InvRouterMirrors` | At every instant the router's routes equal the owner's subscriptions if there is an owner, otherwise the stored session's subscriptions if it has expiry > 0, otherwise nothing. |
| `InvQuiescentConsistent` | When no step is in flight, a live owner's routes, local subscriptions and stored subscriptions are equal. |
| `InvExpiryExact` | A stored disconnected session's expiry clock starts at its true disconnect time, including after a crash. |
| `InvProgress` | When time is exhausted and no broker step is enabled, no ended session (disconnected past its expiry, or marked connected with no running connection) and no owner-less stray route remains. This progress check is based on `ENABLED` and backs up the liveness result. |

Liveness is checked under weak fairness of broker-side connection steps, sweeps, flushes and
time (`Spec`). Client actions (connect, subscribe, disconnect) and failures have no fairness.

| Property | Meaning |
|---|---|
| `EventuallyClean == []<>(now < MaxT \/ ~Stale)` | Once time stops advancing, every ended session and every stray route is eventually cleaned up. |

Negative controls. Each must fail.

| Control | Why it must fail |
|---|---|
| `EventuallyClean` under `SpecNoSweepFair` | Without fairness on sweeps an expired session may never be removed. |
| `NegTimeNeverEnds == []<>(now < MaxT)` | Time reaches `MaxT` and stays there. Shows the checker reports `[]<>` violations under `Spec`. |
| `NegNoResumeAfterRestart` (invariant) | A connection that crashed with subscriptions is resumed, with those subscriptions, after the restart. Shows the chosen design reaches crash recovery. |
| `NegNoResumingTakeover` (invariant) | A new connection takes over a live session and resumes it with its subscriptions. Shows takeover is reached. |

## Reductions and bounds

- **Symmetry** over `Conns` (`SYMMETRY Sym`) in every safety run. Connection attempts are
  interchangeable: `Init` is symmetric, no action or property names a particular connection,
  and tokens are drawn from a counter rather than from connection identities. Liveness runs do
  not use symmetry.
- **Filters.** The chosen design's main runs use two filters. The ablation, `CURRENT`, `CAS`,
  `CASSPLIT` and liveness runs use one filter. Actions and properties treat filters pointwise
  (add or remove one filter, set equality and inclusion), except the "routes non-empty" guard of
  sweeps, so one filter is enough to exhibit each failure; a counterexample at one filter is a
  real counterexample. The passing runs of the chosen design do not rely on this argument,
  because they also pass at two filters.
- **Time.** `E = 1` (expiry 1 means one tick) and `MaxT` = 2 or 3. `Tick` is disabled while any
  connection is in a release step. This models the disconnect time and the end of the logical
  session as the same instant: the DISCONNECT and the release write the same timestamp.
- **Crashes.** `MaxBoots` = 1 (2 in `SessionOwnership_boots2.cfg`). Connection tokens are not
  reset by a crash.
- **Expiry values.** Session Expiry is 0 or 1 (never "never expires"). Any positive value
  behaves like 1 relative to the modelled time bound.

## Group commit

Making every session write durable with its own fsync under a global lock costs about two orders
of magnitude of throughput. Group commit keeps the guarantee (nothing is acknowledged before it
is durable) and makes concurrent writes durable together in one flush. The constant `GC` selects
it; `GC = "OFF"` is the model without group commit.

### How it is modelled

- **Pending buffer, read-your-writes** (`pbuf`, `Log`). Every session write of the chosen design
  (claim, SUBSCRIBE / UNSUBSCRIBE, DISCONNECT expiry, release, sweep removal) still runs under the
  per-ClientID lock. It updates the in-memory record (`cache`) at once and appends the new record
  to the ordered pending buffer. Every reader under the lock sees the latest pending state
  (`View` is the cache while anything is pending), so a claim reads a racing clean start that
  is not yet on disk.
- **Flush** (`GFlushPrefix(k)`). A flush makes the first `k` pending entries durable: the file
  becomes the record of entry `k` (a write replaces the ClientID's whole record, so the last entry
  of the prefix is the file content). `GC = "WHOLE"` flushes only the whole buffer.
  `GC = "PREFIX"` flushes any prefix, which is what a batch does when it snapshots the buffer
  while later writes keep arriving.
- **Acknowledgements wait for the flush** (`AckReady`). `wpos[c]` is the position of connection
  `c`'s latest write in the buffer, 0 once it has been flushed. CONNACK with Session Present
  (`ConnackOK`), SUBACK / UNSUBACK (`SubAck`: `LSub` now ends in `suback`) and the end of
  DISCONNECT processing (`RelDone`: `LRelease` now ends in `relwait`) are enabled only when
  `wpos[c] = 0`, that is, when a flush has covered that write and every earlier pending write.
  A claim that read another connection's unflushed write therefore cannot acknowledge before
  that write is durable.
- **Crash** (`Restart`) loses the whole pending buffer. The file keeps what was flushed.
- **Ghost rollback.** The logical session changes at the write (so a claim that reads a pending
  write is checked against it). Each entry records the logical session after its write, and `dg`
  is the logical session of the file content. At a crash the logical session rolls back to `dg`:
  the lost writes never happened, which is legitimate only because none of them was
  acknowledged. If an acknowledged write is lost (possible only in the ablations), the rollback
  stops at the newest acknowledged entry instead, so the logical session after the crash keeps
  everything a client was told, and the existing invariants then compare the recovered store
  with it. The end of a connection is a real event and applies to every history (`EndG`): the
  session ended at that time even if the write that recorded it is lost, but a new Session
  Expiry sent in the DISCONNECT is part of the DISCONNECT's write and is lost with it.
- **Time.** `Tick` is disabled while any write is unflushed: a flush completes within one unit
  of Session Expiry time (flushes take milliseconds, expiry is counted in seconds). This extends
  the existing "release and disconnect happen at the same instant" abstraction.
- **Bound.** `MaxPend` bounds the buffer; a write blocks while it is full. Every passing
  group-commit run uses `MaxPend = 8` and checks `InvPendNotFull`, so the bound never blocked a
  write there and the buffer's size comes only from the protocol.

Added properties:

| Property | Meaning |
|---|---|
| `InvAckedStateSurvivesCrash` | No crash loses an acknowledged write: at every `Restart`, no acknowledged entry is newer than the file content. The semantic consequences are checked by the existing invariants against the post-crash logical session: a lost claim or wrong Session Present (`InvSessionPresent`), a lost SUBACKed subscription (`InvAckedSubsDurable`), a lost clean-start discard (`InvNoResurrection`), a lost disconnect or expiry (`InvSessionPresent`, `InvExpiryExact`). The `_core` ablation runs leave this property out to show those consequences. With `WHOLE` and `PREFIX` it holds by construction of `AckReady`; it is the direct detector in the ablations. |
| `InvPendNotFull` | The pending buffer never reaches `MaxPend` (the bound is not a restriction). |
| `InvAckedClaimCorrect` (diagnostic) | Every connection that got its CONNACK had a correct Session Present, resume and subscription set. Used to show that a wrong decision in `NORYW` reaches the client. |

Group-commit variants (`GC`):

| `GC` | Flush | Ack rule | Reads | Must |
|---|---|---|---|---|
| `WHOLE` | whole buffer | after a flush covering the write and every earlier write | latest pending state | pass |
| `PREFIX` | any prefix | same | latest pending state | pass |
| `PERWRITER_MONO` | each entry on its own, any order; writing an entry drops every older pending entry, which is then never written (the newer record already carries it) | after the connection's own entry is flushed or dropped as covered | latest pending state | pass |
| `PERWRITER` (ablation a) | each entry on its own, any order; the file gets whatever was flushed last | after the connection's own entry is flushed, earlier entries ignored | latest pending state | fail |
| `ACKEARLY` (ablation b) | any prefix | no wait | latest pending state | fail |
| `NORYW` (ablation c) | any prefix | same as `PREFIX` | the file only | fail |

### Ordering rule for the implementation

1. Under the per-ClientID lock, a session write updates the in-memory session (so every later
   reader under the lock sees it) and is given the next sequence number in lock order.
2. A flush writes and fsyncs every pending write up to some sequence number `N` and only then
   publishes "durable through `N`". The file for a ClientID never goes back to an older version:
   a record is never written over a newer one.
3. CONNACK, SUBACK / UNSUBACK and the completion of DISCONNECT processing for a write with
   sequence number `s` wait until "durable through" is at least `s`. Because `s` was assigned
   after everything the connection read under the lock, this also covers every write the
   decision depended on (for example a racing clean start). A step that acknowledges after a
   pure read would have to wait for the sequence number of the newest write it could see (not
   exercised by the model: every acknowledgement here follows a write of its own).
4. Reads under the lock use the in-memory state, never only the file.

Within one ClientID a write replaces the whole record, so the record of write `s` already
carries every earlier write it read (a claim that resumed after a racing clean start stores the
clean start's result). Flushing only the connection's own write is therefore not unsafe by
itself: `PERWRITER_MONO` passes, and it is the same thing as a prefix flush, since writing entry
`s` with no regression makes everything up to `s` durable. What fails in `PERWRITER` is the
missing no-regression half of rule 2: an earlier write still in flight lands after the later,
acknowledged one and overwrites it. Waiting for "durable through `s`" over a prefix (rule 3)
rules this out by construction, because nothing up to `s` is still in flight when the
acknowledgement is sent. The model has one ClientID; under the "Not modelled" assumption that
different ClientIDs never read each other's records, no order between ClientIDs is needed.

## Runs

Every run was executed with TLC. Every passing run explored its full reachable state space and
every failing run stopped at a counterexample; no run hit a limit or was cut short. Command:

```
java -XX:+UseParallelGC -Xmx6g -cp tla2tools.jar tlc2.TLC -workers 4 \
  -metadir <scratch>/states/<run> -config <cfg> SessionOwnership.tla
```

Common constants: `E = 1`, `None = none`. "Conns" is the number of connection attempts. Every
run except the `_GC_*` runs uses `GC = "OFF"`, `MaxPend = 1`. All runs below were repeated
against the group-commit spec: every passing run has the same distinct-state count as before
group commit was added, and every failing run fails on the same property except
`_ABL_wt_disc` (see the note under the negative controls).

### Chosen design (`MECH = "LOCK"`, `DROP = {}`)

| cfg | Conns | Filters | MaxT | MaxBoots | Reductions | Checks | Result | Distinct states | Depth | Time |
|---|---|---|---|---|---|---|---|---|---|---|
| `SessionOwnership.cfg` | 3 | 2 | 3 | 1 | symmetry | all 14 invariants | pass | 4,138,304 | 24 | 91 s |
| `SessionOwnership_boots2.cfg` | 3 | 2 | 3 | 2 | symmetry | all 14 invariants | pass | 7,649,114 | 25 | 176 s |
| `SessionOwnership_live.cfg` | 3 | 1 | 2 | 1 | none | `EventuallyClean` under `Spec` | pass | 2,654,334 | 20 | 145 s |

### Negative controls

| cfg | Conns | Filters | MaxT | MaxBoots | Reductions | Expected | Result | Distinct states | Depth | Time | Counterexample |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `SessionOwnership_NEG_nosweepfair.cfg` | 3 | 1 | 2 | 1 | none | liveness failure | `EventuallyClean` violated | 81,717 | 10 | 3 s | c3's expiry-0 session is still connected when the broker crashes. It is stamped as ended at boot, expires one tick later, and without sweep fairness it is never removed. |
| `SessionOwnership_NEG_timeneverends.cfg` | 3 | 1 | 2 | 1 | none | liveness failure | `NegTimeNeverEnds` violated | 85,406 | 10 | 3 s | Time reaches `MaxT`. |
| `SessionOwnership_NEG_resumeafterrestart.cfg` | 3 | 1 | 2 | 1 | symmetry | violation | `NegNoResumeAfterRestart` violated | 3,824 | 10 | 1 s | c1 claims with expiry 1 and subscribes, the broker crashes, c2 claims and resumes c1's session with its subscription. |
| `SessionOwnership_NEG_resumingtakeover.cfg` | 3 | 1 | 2 | 1 | symmetry | violation | `NegNoResumingTakeover` violated | 1,241 | 9 | 1 s | c2 is live with a subscription, c3 takes over and resumes it. |

For a failing run, the state count is what TLC had explored when it found the violation. With 4
workers it varies slightly between runs, and when two counterexamples have the same length the
one reported first can differ. For example, `_ABL_wt_disc` has reported both `InvSessionPresent` and `InvExpiryExact` (the
final run reports `InvExpiryExact`).

### Ablations of the chosen design (`MECH = "LOCK"`, 3 Conns, 1 filter, `MaxT = 2`, `MaxBoots = 1`, symmetry)

Each ablation checks all 14 invariants (unless noted). BFS gives the shortest counterexample, so
"first violated" is the property that breaks earliest.

| cfg (`DROP`) | Expected | First violated | Distinct states | Depth | Time | Counterexample |
|---|---|---|---|---|---|---|
| `_ABL_claim` (`claim`) | Session Present or ownership | `InvSessionPresent` | 62 | 7 | 0 s | c1 writes its session but has not registered or sent CONNACK. c3 reads it and resumes (SP = 1) a session with expiry 0 that belongs to a connection still in its handshake. Without the claim, "who owns the session" and "what did the session look like" are decided at different moments. |
| `_ABL_sub` (`sub`) | router mirror | `InvRouterMirrors` | 101 | 7 | 1 s | The router route is added before the store write. |
| `_ABL_sub_core` (`sub`, without `InvRouterMirrors`) | ownership | `InvOwnershipAgree` | 1,474 | 9 | 0 s | c2 adds a route, c3 takes over, then c2's delayed store write overwrites the whole session with c2's token: the store now names a displaced connection. |
| `_ABL_disc` (`disc`) | Session Present | `InvSessionPresent` | 1,069 | 8 | 1 s | c1 (expiry 1) disconnects with expiry 0. Before its release runs, c2 claims and resumes the stored session, which still says expiry 1, although the session ended at the DISCONNECT. |
| `_ABL_release` (`release`) | router mirror | `InvRouterMirrors` | 834 | 8 | 1 s | Router release and route strip are separate steps. |
| `_ABL_release_core` (`release`, without `InvRouterMirrors`) | quiescent consistency | `InvQuiescentConsistent` | 71,829 | 13 | 2 s | c1's handshake fails and it releases the router. c2 claims and subscribes. c1's delayed strip then deletes c2's routes. |
| `_ABL_sweep` (`sweep`) | live owner intact | `InvLiveOwnerSessionKept` | 14,263 | 11 | 1 s | After a restart the file sweep decides a session is expired. c3 claims a new session. The sweep then deletes c3's live session without re-checking. |
| `_ABL_stamp` (`stamp`) | exact expiry | `InvExpiryExact` | 120 | 7 | 1 s | After a crash the loaded session's expiry counts from the time it is read, not from when it ended. |
| `_ABL_rebuild` (`rebuild`) | router mirror | `InvRouterMirrors` | 810 | 8 | 0 s | c3 (expiry 1) subscribes, the broker crashes, and the router comes back without the stored session's routes, so offline messages are not routed. |
| `_ABL_wt_claim` (`wt_claim`) | Session Present | `InvSessionPresent` | 453 | 7 | 1 s | c1's claim with expiry 1 is acknowledged but still in the write-behind cache when the broker crashes. c2 gets SP = 0 for a session that should still exist. |
| `_ABL_wt_claim_resurrect` (`wt_claim`, only `InvNoResurrection`) | resurrection across a crash | `InvNoResurrection` | 6,163 | 10 | 1 s | c1's session (expiry 1) is flushed. c2 connects with Clean Start = 1; the discard is still in the cache when the broker crashes. c3 resumes c1's discarded session. |
| `_ABL_wt_sub` (`wt_sub`) | acked subscriptions durable | `InvAckedSubsDurable` | 3,542 | 9 | 1 s | c1's SUBACK is sent, the broker crashes before the flush, and c2 resumes without the subscription. |
| `_ABL_wt_disc` (`wt_disc`) | Session Present or exact expiry | `InvExpiryExact` | 2,696 | 9 | 0 s | c3 (expiry 1) fails its CONNACK; its release marks the session disconnected at time 0 in the cache only. Time advances and the broker crashes: the file still says connected, so the boot stamps the disconnect at time 1 and the expiry clock starts late. |
| `_ABL_wt_remove` (`wt_remove`) | resurrection of a removed session | `InvExpiryExact` | 896 | 8 | 1 s | c3's expiry-0 session (its CONNACK failed) is removed in the cache only. After a crash the file brings it back with a wrong disconnect time. |
| `_ABL_wt_remove_core` (`wt_remove`, without `InvExpiryExact`) | ended session never cleaned | `InvProgress` | 2,582 | 9 | 1 s | The same lost removal: the resurrected session outlives its true end and nothing removes it in time. |

### Today's code (`MECH = "CURRENT"`, 3 Conns, 1 filter, `MaxT = 2`, `MaxBoots = 1`, symmetry)

Each cfg checks `TypeOK` and one property. Liveness is checked without symmetry.

| cfg | Result | Distinct states | Depth | Time | Counterexample |
|---|---|---|---|---|---|
| `_CUR_OwnershipAgree` | `InvOwnershipAgree` violated | 513 | 8 | 0 s | c1 registers. c2, whose read happened earlier, writes its own session: the store names c2 while the router names c1. |
| `_CUR_NoLeakedConnected` | `InvNoLeakedConnected` violated | 23,722 | 12 | 1 s | c1 and c2 both write and register; c1 registers last. c2 disconnects with expiry 0 and, no longer owner, skips cleanup. c1's record was overwritten by c2's, which stays "connected" with no connection. |
| `_CUR_LiveOwnerSessionKept` | `InvLiveOwnerSessionKept` violated | 3,813 | 10 | 1 s | c1 is the live owner. c2 overwrites the store, fails its CONNACK and marks the record disconnected: the live owner's session is gone. |
| `_CUR_FailedHandshakeHarmless` | `InvFailedHandshakeHarmless` violated | 3,116 | 10 | 1 s | Same interleaving: a failed handshake leaves its own token in the live owner's session. |
| `_CUR_SessionPresent` | `InvSessionPresent` violated | 76 | 6 | 0 s | c2 resumes (SP = 1) the expiry-0 session c1 wrote during its handshake. |
| `_CUR_NoResurrection` | `InvNoResurrection` violated | 684 | 8 | 1 s | c3 reads c1's session. c2 then writes a fresh session. c3 writes back the resumed c1 session, undoing c2's fresh start. |
| `_CUR_AckedSubsDurable` | `InvAckedSubsDurable` violated | 36,779 | 12 | 1 s | c1 is the live owner and subscribes (SUBACK sent, stored). c2, still in its handshake, overwrites the store with the fresh empty session it decided on earlier. c3 then resumes that record, which lacks c1's acknowledged subscription. |
| `_CUR_CleanStart` | `InvCleanStart` violated | 2,643,432 | 18 | 26 s | c2 starts empty and subscribes. c3 resumes c2's session and registers, but its route install is still pending. c1, which wrote an empty session long before, now registers (clean, so it strips the routes). c3's late `Install` then unions c2's route into the router while c1, a clean owner with no subscriptions, owns it. |
| `_CUR_RouterMirrors` | `InvRouterMirrors` violated | 316 | 8 | 1 s | A route is added before the store write. |
| `_CUR_QuiescentConsistent` | `InvQuiescentConsistent` violated | 55,221 | 12 | 1 s | c1 subscribes and persists. c2, whose store write came earlier, registers: c2's router entry and stored session disagree. |
| `_CUR_ExpiryExact` | `InvExpiryExact` violated | 522 | 8 | 1 s | A connected session is flushed, the broker crashes, and the loaded session's expiry counts from the read time. |
| `_CUR_Progress` | `InvProgress` violated | 1,736 | 9 | 0 s | c1's expiry-0 session is flushed while connected and the broker crashes. With no stored disconnect time, the loaded session's expiry counts from the current time on every read, so it never expires and is never removed, although it ended at the crash. |
| `_CUR_live` (no symmetry) | `EventuallyClean` violated | 92,090 | 10 | 4 s | The same session is never cleaned up. |

### Rejected lock-free designs (3 Conns, 1 filter, `MaxT = 2`, `MaxBoots = 1`, symmetry)

| cfg | MECH | Checks | Result | Distinct states | Depth | Time | Counterexample |
|---|---|---|---|---|---|---|---|
| `_CAS_agree` | `CAS` | `InvOwnershipAgree` | violated | 27 | 5 | 0 s | c1, c2 and c3 claim in storage in turn. c1 registers at the router while the store holds c3's token. |
| `_CAS_mirrors` | `CAS` | `InvRouterMirrors` | violated | 130 | 7 | 1 s | The router step of a SUBSCRIBE runs before its fenced store step. |
| `_CAS_rest` | `CAS` | the other 12 invariants | pass | 1,887,418 | 30 | 31 s | none: hardened CAS is correct except for the two properties above. |
| `_CASSPLIT_agree` | `CASSPLIT` | `InvOwnershipAgree` | violated | 37 | 5 | 0 s | c1 and c2 register at the router in turn (c2 is owner). c1's storage claim then writes c1's token. |
| `_CASSPLIT_resurrect` | `CASSPLIT` | `InvNoResurrection` | violated | 5,542 | 8 | 1 s | **Resurrection.** c1 registers (expiry 1). c2 registers with Clean Start = 1, which strips the routes. c3 registers (Clean Start = 0). c1's storage claim writes c1's session. c3's storage claim passes the token compare (c1's token is older) and resumes c1's session, which c2's clean start, registered before c3, had discarded. |

### Group commit (`MECH = "LOCK"`, `DROP = {}`, `MaxPend = 8`, symmetry except liveness)

"All 16" is the 14 invariants of the chosen design plus `InvAckedStateSurvivesCrash` and
`InvPendNotFull`.

| cfg | GC | Conns | Filters | MaxT | MaxBoots | Reductions | Checks | Result | Distinct states | Depth | Time |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `SessionOwnership_GC_whole.cfg` | `WHOLE` | 3 | 2 | 3 | 1 | symmetry | all 16 invariants | pass | 26,003,764 | 42 | 519 s |
| `SessionOwnership_GC_prefix.cfg` | `PREFIX` | 3 | 2 | 3 | 1 | symmetry | all 16 invariants | pass | 26,003,764 | 42 | 544 s |
| `SessionOwnership_GC_prefix_boots2.cfg` | `PREFIX` | 3 | 2 | 3 | 2 | symmetry | all 16 invariants | pass | 47,933,876 | 43 | 1,006 s |
| `SessionOwnership_GC_prefix_live.cfg` | `PREFIX` | 3 | 1 | 2 | 1 | none | `EventuallyClean` under `Spec` | pass | 15,768,372 | 32 | 1,182 s |
| `SessionOwnership_GC_perwriter_mono.cfg` | `PERWRITER_MONO` | 3 | 1 | 2 | 1 | symmetry | all 16 invariants | pass | 2,628,548 | 32 | 50 s |

`WHOLE` and `PREFIX` reach the same set of states: a partial flush leaves the same buffer as a
whole flush taken before the later writes were appended.

Group-commit ablations (3 Conns, 1 filter, `MaxT = 2`, `MaxBoots = 1`, symmetry). Each must fail.
`_core` repeats the ablation with the 14 invariants of the chosen design only (without
`InvAckedStateSurvivesCrash`), to show the MQTT-visible consequence.

| cfg | GC | Checks | First violated | Distinct states | Depth | Time | Counterexample |
|---|---|---|---|---|---|---|---|
| `_GC_ABL_perwriter` | `PERWRITER` (a) | all 16 | `InvAckedStateSurvivesCrash` | 1,219 | 8 | 1 s | c1, c2, c3 claim in turn (three pending entries). c2's entry is flushed and c2 gets its CONNACK. c1's older entry is flushed afterwards and overwrites the file with c1's record. The broker crashes: c2's acknowledged claim is gone. |
| `_GC_ABL_perwriter_core` | `PERWRITER` (a) | the 14 | `InvSessionPresent` | 10,036 | 9 | 1 s | c1 claims with expiry 1; c2 takes over (Clean Start = 0, expiry 0). c2's entry is flushed and c2 gets CONNACK. c1's older entry then lands on the file. After a crash c3 resumes c1's expiry-1 session (SP = 1), although the session c2 was told about had expiry 0 and ended at the crash. |
| `_GC_ABL_ackearly` | `ACKEARLY` (b) | all 16 | `InvAckedStateSurvivesCrash` | 112 | 6 | 0 s | c1, c2, c3 claim; c1 gets CONNACK while its entry is still pending; the broker crashes. |
| `_GC_ABL_ackearly_core` | `ACKEARLY` (b) | the 14 | `InvSessionPresent` | 1,401 | 8 | 1 s | c1 claims with expiry 1 and gets CONNACK before the flush; the broker crashes; c2 (Clean Start = 0) gets SP = 0 for the session c1 was told exists. |
| `_GC_ABL_noryw` | `NORYW` (c) | all 16 | `InvSessionPresent` | 68 | 6 | 0 s | c1 claims with expiry 1 (pending). c2 (Clean Start = 0) reads only the file, finds nothing and starts empty (SP = 0), discarding c1's session. No crash is involved. |
| `_GC_ABL_noryw_core` | `NORYW` (c) | the 14 | `InvSessionPresent` | 68 | 6 | 1 s | c2's expiry-1 claim is pending; c3 (Clean Start = 0) reads the file, starts empty and discards it. |
| `_GC_ABL_noryw_acked` | `NORYW` (c) | `TypeOK`, `InvAckedClaimCorrect` | `InvAckedClaimCorrect` | 793 | 8 | 0 s | The wrong decision reaches the client: after the stale claim above, both entries are flushed in order and c2 gets CONNACK with SP = 0 for a session that existed. |
| `_GC_ABL_noryw_subs` | `NORYW` (c) | `TypeOK`, `InvAckedSubsDurable` | `InvAckedSubsDurable` | 2,972 | 9 | 1 s | c2 (expiry 1) is live and subscribes; the SUBSCRIBE is pending. c3 takes over and resumes from the file, without the subscription. c2's entry precedes c3's, so c2's SUBACK can still be sent after the flush: an acknowledged subscription is lost without any crash. |

Verdict on (c): reads that see only the file are unsafe, not merely stale. The lock orders the
claim after the pending write and the flush makes both durable in that order, so the stale
decision is final and it contradicts a write that precedes it (and that may be acknowledged).

## Provenance

Three modelers built independent models, checked exhaustively with TLC, and reached the same
verdict: the per-ClientID lock.

- **Modeler 1** (`SessionMech`, `MECH` in `LOCK`, `CAS`, `CASSPLIT`, `CURRENT`, `CUSTOM`):
  per-fix ablations and the router/store single-writer check. It found the `CASSPLIT`
  resurrection (a clean start that registered first is undone by a later storage claim) and
  that the clean-start discard must be durable (`durable_clean`), otherwise a crash resurrects
  the discarded session (`InvNoResurrect`).
- **Modeler 2** (`SessionMech`, `SessionMechAnom`, `SessionMechDur`; `LOCK` vs hardened `CAS` vs
  `FENCE`): Session Present linearizability (`InvSpLinearizable`), the always-mirror router
  property (`InvRouterAlwaysMirrors`, which hardened CAS fails), the acked-clean-start-lost
  anomaly of `FENCE` (`InvNoAckedCleanStartLost`), and a 16-way durability sweep over claim,
  exit, remove and subscription writes showing that only "all durable" passes.
- **Modeler 3** (`SessionOwnership` first round, then `SessionMech` with `LOCK` vs
  `CAS_RF` / `CAS_RF2` / `CAS_SF`): the file backend with cache, write-behind flush and restart,
  exact expiry across restart (persisted timestamp plus boot stamping), claim linearizability
  (`InvClaimLinearizable`), the `CURRENT` per-property counterexamples, and the hardening a CAS
  design needs (token epochs, tombstones, displaced cleanup, fenced router sweeps).

This consolidated spec is built on modeler 3's file-backend model (time, cache, write-behind,
restart, ghost logical session). Merged in: modeler 1's mechanism switch with per-fix ablation
and its `CASSPLIT` resurrection; modeler 2's always-mirror router property and its per-write
durability ablation (here `wt_claim`, `wt_sub`, `wt_disc`, `wt_remove`); and modeler 3's own
first-round `CURRENT` model as the "element dropped" behaviour. Names were unified:
`InvClaimLinearizable` / `InvNoResurrect` / `InvSpLinearizable` became `InvNoResurrection` and
`InvSessionPresent`; `InvRouterAlwaysMirrors` became `InvRouterMirrors`. The hardened `CAS` here
is modeler 3's `CAS_SF` with all hardening on; `CASSPLIT` is modeler 3's `CAS_RF` without the
storage tombstone, which is modeler 1's `CASSPLIT`.

## Tool notes

- All results come from the TLC command line. tla-mcp 0.10.1 was not used for any reported
  result because:
  - `P ~> Q` can pass vacuously,
  - `[](P => <>Q)` is not supported,
  - it ignores the cfg's `SPECIFICATION`, so a negative control that swaps in a spec without
    sweep fairness would silently run with the fair spec.

  TLC honours `SPECIFICATION`, which the failing `SpecNoSweepFair` control demonstrates.
- Liveness is written only as `[]<>` over state predicates, never with `~>`.
- State directories were kept outside the repository and deleted after each run. A failing run
  also makes TLC write `SessionOwnership_TTrace_*.tla` / `.bin` files next to the spec; those
  were deleted and are not part of this directory.

## Mapping from spec actions to code

The code is being reworked alongside this model; names are as of this modelling pass.

| Spec | Broker concept |
|---|---|
| per-ClientID critical section (every `L*` action) | the session slot lock: `SessionSlots` / `MessageRouter::lock_session`, held for the whole step |
| `LClaim` | the claim in the CONNECT path (`handle_session` in `client_handler/connect.rs`), under the session slot: `get_session`, then a durable session write (`write_claim`), then `register_session_as` and an exact router route set (`set_client_subscriptions`), all before CONNACK. If the write fails the claim stops before registering and the client gets CONNACK 0x88 |
| `tok` | `ClientSession::connection_token` (the router generation) |
| `Owns(c)` | the token compare in `update_session` / `remove_owned_session` |
| `LSub`, `SubRouter` / `SubStore` | `subscribe_as` / `unsubscribe_as` for every filter of the packet, then one owner-only `update_session` carrying all of them (`persist_or_restore_routes`), durable before SUBACK / UNSUBACK, versus today's router subscribe followed by a whole-session `store_session` |
| `Close` with `disc` | DISCONNECT packet handling: under the per-ClientID lock, at the DISCONNECT and before the release, if the DISCONNECT changes Session Expiry and the connection still owns the stored session (token compare), write the new expiry to storage (with group commit: append it to the pending buffer). The release (`LRelease`) is a separate, later critical section that writes the disconnect time or removes the session. |
| `LRelease`, `Release` / `Strip` / `Cleanup` / `RmOwned` | `release_client` / `release_ownership` / `persist_session_end` (owner-only, strip under the lock) versus today's `unregister_client`, route strip, `update_session` and `remove_owned_session` as separate awaits |
| `ConnackFail`, `HConnackFail` / `FailRel` | a CONNACK write that fails after the claim |
| `LSweep`, `FileSweepRead` / `FileSweepApply`, `RouterSweepDecide` / `RouterSweepApply` | `sweep_sessions` / `sweep_session` under the slot versus today's `cleanup_expired` (file sweep) and `cleanup_stale_subscriptions` (router sweep) |
| `Wr(_, TRUE, _)`, `Remove(TRUE)` | write-through session writes and removals |
| `Wr(_, FALSE, _)`, `Flush` | today's write-behind `store_session` and the periodic `flush_sessions` |
| `pbuf`, `Log` (group commit) | the group-commit pending buffer: each session write, under the per-ClientID lock, updates the in-memory cache and appends the new record with the next sequence number |
| `GFlushPrefix(k)` | one group-commit batch: write and fsync every record up to sequence number k, then publish "durable through k" |
| `AckReady`, `ConnackOK`, `SubAck`, `RelDone` | the wait before CONNACK, SUBACK / UNSUBACK and the end of DISCONNECT processing: the connection waits until "durable through" is at least the sequence number of its latest write |
| `seen`, `ts` | `disconnected_at` (`ClientSession::mark_disconnected`) |
| `Restart` with `stamp` and `rebuild` | `recover_sessions`: stamp still-connected sessions with the boot time, rebuild router routes from stored sessions with expiry > 0. The code also removes expired and expiry-0 sessions during recovery; the model leaves them to the sweep, whose re-check removes them the same way. |
| `SStore` / `SReg`, `RReg` / `RStore` / `RInstall`, `CRelease` / `CClean`, `CFileSweep` | the rejected lock-free designs; no code |

## Not modelled

- **Queues and inflight state.** Offline message queues, QoS 1/2 inflight and packet ids are not
  modelled (see `specs/tla/offline-queue/` and `specs/tla/deferred-ack/`). The session record
  here holds only ownership, subscriptions, expiry and timestamps.
- **Wills.** Will messages are not modelled. The implementation fences will claims with the same
  connection generation that fences session writes (`claim_will`); that token compare is the
  only part of will handling this model's `Owns` check stands for.
- **Multiple ClientIDs.** One ClientID only. Different ClientIDs use different locks and
  different records and do not interact in this protocol.
- **Subscription options, shared subscriptions, retained messages, authentication.**
- **Memory backend.** Only the file backend is modelled. The memory backend is the file backend
  without a file: nothing survives a restart.
- **Failed writes.** Every write in the model succeeds. What the code does when one fails is
  covered by tests, not by the model: a failed flush rejects every write pending with it and
  restores the last durable state, the log is repaired before the next append, a failed claim
  write is refused with CONNACK 0x88 without displacing the live owner, and a failed SUBSCRIBE or
  UNSUBSCRIBE write restores the routes and the stored subscriptions and ends the connection
  with DISCONNECT 0x80.
- **Bounds.** Results hold for the constants listed, not in general.
