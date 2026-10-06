# Storage directory lock and shutdown ordering

Model for #179: when may a broker release the lock on its storage directory, and when may `MqttBroker::run()` return, so that no two brokers write `sessions/sessions.log` at once and no session write is lost.

`StorageLock.tla` has one old broker with connection handlers that write their session-end record after the shutdown signal, a new broker (in-process restart or another process) that takes the lock whenever it is free, and an optional process exit after `run()` returns that cuts off unfinished writes.

Variants:

- `RELEASE`: `SHUTDOWN` releases the lock when storage shutdown runs on the signal; `DROP` releases it when the backend is dropped, after the last handler has finished.
- `RUNWAIT`: `NO` lets `run()` return without waiting for handlers; `YES` makes it wait for them.
- `TIMEOUT`: with `RUNWAIT = YES`, whether the bounded wait can expire first.

Properties:

- `InvExclusive`: no old handler writes while the new broker holds the lock.
- `InvDurableAtReturn`: when `run()` returns, every session-end write is done.
- `InvNoLostWrites`: a process exit after `run()` returns loses no write.
- `NewEventuallyRuns`: the new broker eventually runs.

Results with `Conns = {c1, c2, c3}` and symmetry, every run exhaustive:

| Variant | Exclusive | DurableAtReturn | NoLostWrites | NewEventuallyRuns |
|---|---|---|---|---|
| SHUTDOWN_NOWAIT | violated | violated | violated | holds |
| DROP_NOWAIT | holds | violated | violated | holds |
| DROP_WAIT | holds | holds | holds | holds |
| DROP_WAIT_TIMEOUT | holds | violated (timeout only) | violated (timeout only) | holds |
| SHUTDOWN_WAIT | violated | holds | holds | holds |

Controls: `StorageLock_NEG_NEVERRELEASE_Live.cfg` (lock never released) violates `NewEventuallyRuns`, and `NEG_ReturnedWithAllWrittenUnreachable` is violated in DROP_WAIT and SHUTDOWN_WAIT, so the passing liveness and durability results are not vacuous.

The implementation is DROP_WAIT: the file backend releases the lock in `Drop`, and `run()` waits for every connection handler. The 5 second bound on that wait is DROP_WAIT_TIMEOUT: exclusivity still holds, and durability is lost only if the wait expires, which `run()` logs.
