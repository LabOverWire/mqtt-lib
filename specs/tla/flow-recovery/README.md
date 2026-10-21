# QUIC flow recovery

Model for #252: how the QUIC broker answers a client data flow opened with the `clean` and `abort_if_no_state` flags (MQoQ §9.8, §9.15), and where it keeps flow state so that an aborted flow keeps its state until it expires (§9.13), across a closed connection and a session takeover.

`FlowRecovery.tla` has one flow ID, two connections of the same client (a reconnect or a takeover), and up to `MaxStreams` streams that open the flow. Each stream carries `clean`, `abort_if_no_state` and a flow expire interval of 0 or more. The client subscribes on a stream, ends it with FIN or reset, or the connection closes. `Tick` is the expire interval passing for a kept flow.

`abs` is the flow state MQoQ asks for, updated where the broker first sees each event: the stream header, the stream end, the connection close, a clean start. It has the flow's state (`none`, `open` or `kept`), the stream it is open on, a generation that counts fresh starts, and the generations its subscriptions belong to. The broker's own state is `reg` (the flow registry), `subs` (generations of the flow-bound subscriptions in the router) and `queue` (events waiting for a client handler).

Rules from the spec, computed from `abs` when a header arrives:

- `clean` and `abort_if_no_state` both set: protocol error level 1, the state is discarded and the flow refused.
- `clean` set: the previous state is discarded and the flow starts fresh.
- `clean` unset and state kept or open: the flow is recovered.
- `clean` unset, no state, `abort_if_no_state` set: refused with ERROR_NO_FLOW_STATE, and a second attempt on that connection closes it with ERROR_TOO_MANY_RECOVER_ATTEMPTS.
- `clean` unset, no state, `abort_if_no_state` unset: the flow starts fresh.

Variants:

- `STORE`: `CONN` keeps the registry per connection, as the broker did; `SESSION` keeps one registry for the session.
- `FLAGS`: `IGNORE` serves every header as a fresh flow; `HONOR` applies the rules above to the broker's registry.
- `DISCARD`: `ACKED` removes the old subscriptions before the new stream is served; `ASYNC` queues their removal for the client handler.
- `LOOKUP`: `TIMER` treats a kept flow as recoverable until its expiry timer has run; `DEADLINE` checks the deadline.
- `TEARDOWN`: `CLAIM` lets the connection that claims the session convert the open flows of the connection it replaces (kept if their expire interval is above 0, removed otherwise), as the owner does when it releases the session; `OWNER` leaves that to the old connection's handler, which may no longer own the session.

With `STORE = CONN` the registry, stream ends and teardown follow the broker before this change: a stream end reports only the flow ID, the expiry timer runs in the stream reader and stops when the connection closes, and a handler that was taken over skips its flow cleanup.

Properties:

- `InvResponses`: every header gets the answer the rules give for `abs`.
- `InvNotEarly`: the broker never removes a subscription that `abs` still has (§9.13).
- `InvSettled`: once nothing is pending (no unread header or stream end, no queued event, no connection waiting for teardown, no timer due), the broker's subscriptions are exactly those of `abs`, so nothing stale from an expired, discarded or replaced flow is left.

Results with `MaxStreams = 3`, every run exhaustive:

| Variant | STORE | FLAGS | DISCARD | LOOKUP | TEARDOWN | Responses | NotEarly | Settled |
|---|---|---|---|---|---|---|---|---|
| CUR | CONN | IGNORE | | | | violated | violated | violated |
| CONN_HONOR | CONN | HONOR | ACKED | DEADLINE | | violated | violated | violated |
| FIX_ASYNC | SESSION | HONOR | ASYNC | DEADLINE | CLAIM | holds | violated | violated |
| FIX_TIMER | SESSION | HONOR | ACKED | TIMER | CLAIM | violated | holds | violated |
| FIX_NOCLAIM | SESSION | HONOR | ACKED | DEADLINE | OWNER | violated | holds | violated |
| FIX | SESSION | HONOR | ACKED | DEADLINE | CLAIM | holds | holds | holds |

The counterexamples:

- CUR, Responses: a header with `clean` and `abort_if_no_state` is served.
- CUR, Settled: a `clean` restart keeps the subscriptions of the flow it replaces.
- CUR, NotEarly: a FIN report for an older stream is handled after the connection closed and removes the subscriptions of the newer, kept stream of the same flow, since the report carries only the flow ID.
- CONN_HONOR: after a reconnect the new connection has no registry, so it refuses (or restarts fresh, dropping the subscriptions of) a flow whose state is kept; on a takeover the expire-0 flow's subscriptions carry over.
- FIX_ASYNC: the queued removal runs after the client subscribed on the new stream.
- FIX_TIMER: a recovery that arrives after the deadline but before the timer ran is served with the expired state.
- FIX_NOCLAIM: the old connection's open flow is never converted after a takeover, so a recovery on the new connection finds it open.

`FlowRecovery_FIX_4.cfg` checks FIX with all three properties at `MaxStreams = 4`: they hold, 139,848,332 distinct states, depth 25, exhaustive.

Controls, each violated in FIX, so the passing results are not vacuous: `NEG_KeptWithSubsUnreachable` (a kept flow with a subscription), `NEG_NoStateRefusalUnreachable` (an ERROR_NO_FLOW_STATE refusal on the second connection) and `NEG_RecoveryAcrossConnectionsUnreachable` (a flow started on the first connection recovered with its subscription on the second).

What the model assumes of an implementation of FIX:

- One writer decides every flow open and end in order: the client handler. A stream reader sends the header to the handler and waits for the answer before it reads packets, and sends the stream end on the same channel as the stream's packets, so a SUBSCRIBE on a stream is handled before that stream's end. `Subscribe` is one step in the model for that reason.
- The registry belongs to the session and survives the connection. Claiming the session and releasing it both convert the open flows of connections that are gone, under the session lock, and a connection that was taken over no longer changes the registry.
- A lookup compares a kept flow's deadline with the clock; the expiry timer runs per session, also while no connection is up, and only removes the flow it was set for.
