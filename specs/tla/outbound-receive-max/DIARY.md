# Outbound Receive Maximum — TLA+ diary

Newest entries on top.

## Planned / future stages

- Stage 2: add network loss + duplication (bag semantics), resend of stored unacked publishes.
- Stage 3: reconnect / session-resume — re-prime the outbound window to the new CONNACK
  Receive Maximum and re-acquire permits while replaying stored unacked publishes; prove
  `Cardinality(SpecUnacked) <= SRM'` holds mid-replay.
- Liveness (needs fairness WF on the ack actions): every acquired permit is eventually
  released, so a full window always drains. Deadlock-freedom is already checked as safety
  (`allow_deadlock = false` passed on the fixed model).

## 2026-08-16 — Stage 1: release-point safety, verdict

`OutboundReceiveMax.tla` models the client publishing QoS2 messages gated by a send-quota
permit bounded by the broker's advertised Receive Maximum (`SRM`). `permitHeld` is the
implementation's accounting (the gate); `SpecUnacked` is MQTT-5 §4.9's definition of
"unacknowledged" (sent, not yet PUBCOMP). The safety invariant `InvWindowBound` is stated
over `SpecUnacked`, so any release point that lets `permitHeld` diverge from `SpecUnacked`
is caught. The constant `ReleaseOnPubrec` selects the buggy vs correct release point.

Runs (TLC via tla mcp):

- `OutboundReceiveMax_fixed.cfg` (`ReleaseOnPubrec = FALSE`, release on PUBCOMP), SRM=2,
  MaxMsgs=3: **status ok**, 98 states, no deadlock. `TypeOK`, `InvWindowBound`,
  `InvPermitBound`, and `InvPermitTracksUnacked` (`permitHeld = SpecUnacked`) all hold.
- `OutboundReceiveMax_buggy.cfg` (`ReleaseOnPubrec = TRUE`, release on success PUBREC),
  SRM=2, MaxMsgs=3: **InvWindowBound VIOLATED** in 5 steps —
  Send(1); Send(2) [window full]; RecvPubrec(1) frees a permit while msg 1 is still in
  the `pubrec` phase; Send(3) is admitted → `SpecUnacked = {1,2,3}` = 3 > SRM=2.
- Buggy at the tightest boundary SRM=1, MaxMsgs=2: **VIOLATED** in 4 steps
  (Send(1); RecvPubrec(1) frees; Send(2) → SpecUnacked={1,2}=2 > 1).

**Verdict:** the QoS2 send-quota permit MUST be released on PUBCOMP, not on a success
PUBREC. An error PUBREC (reason ≥ 0x80) ends the exchange early and releases the permit
there (`RecvPubrecError`), which the model includes and which preserves the bound. QoS1 is
the degenerate single-release case (release on PUBACK) and is trivially safe.
