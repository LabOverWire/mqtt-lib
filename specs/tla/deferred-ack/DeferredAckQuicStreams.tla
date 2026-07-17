--------------------- MODULE DeferredAckQuicStreams ----------------------
(***************************************************************************)
(* Stage 7: inbound QoS2 over QUIC, where one logical flow spans TWO         *)
(* independently-ordered streams.                                           *)
(*                                                                          *)
(* Established by code trace + quorum (see TLA_DIARY 2026-07-15):            *)
(*   - broker sends PUBLISH on a server-initiated DATA stream                *)
(*     (ServerDeliveryStrategy::PerTopic is the DEFAULT)                     *)
(*   - client writes PUBREC back onto THAT SAME data stream (reader.rs:498   *)
(*     wraps the accepted stream's send half as the writer)                  *)
(*   - broker sends PUBREL on the CONTROL stream (publish.rs:515)            *)
(*   - client sends PUBCOMP on the control stream                            *)
(* QUIC gives NO ordering between the data stream and the control stream.    *)
(*                                                                          *)
(* CONSTANT BrokerReadsDataStream selects the design:                        *)
(*   FALSE -- current code: ServerStreamManager does                          *)
(*        `let (mut send, _recv) = open_bi()` and DROPS the recv half         *)
(*        (server_stream_manager.rs:80/133/174). Nothing reads the client's   *)
(*        PUBREC. Expected: InvHandshakeCanComplete VIOLATED — the flow wedges.*)
(*   TRUE  -- Option B: the broker spawns a reader over each server-opened     *)
(*        stream's recv half. Expected: holds.                                *)
(*                                                                          *)
(* Deliberately NOT modelled: quinn's RecvStream::drop sending STOP_SENDING   *)
(* (which makes the client's write actively fail rather than vanish). That is *)
(* a transport detail below this abstraction; either way the PUBREC never      *)
(* reaches broker logic, which is what this model captures.                    *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
    MaxSends,             \* bound on PUBLISH transmissions (initial + DUP)
    BrokerReadsDataStream \* TRUE = Option B; FALSE = current code

VARIABLES
    brokerState,   \* "toSend" | "awaitPubrec" | "awaitPubcomp" | "done"
    sendCount,
    dataStream,    \* server -> client PUBLISH packets in flight (ordered)
    dataStreamAck, \* client -> server PUBREC in flight ON THE DATA STREAM
    ctrlToClient,  \* server -> client PUBREL in flight (control stream)
    ctrlToServer,  \* client -> server PUBCOMP in flight (control stream)
    pubrecSent,    \* client: inbound_pubrecs marker (the dedup guard)
    deliveryCount

vars == <<brokerState, sendCount, dataStream, dataStreamAck, ctrlToClient,
          ctrlToServer, pubrecSent, deliveryCount>>

Init ==
    /\ brokerState = "toSend"
    /\ sendCount = 0
    /\ dataStream = 0
    /\ dataStreamAck = 0
    /\ ctrlToClient = 0
    /\ ctrlToServer = 0
    /\ pubrecSent = FALSE
    /\ deliveryCount = 0

(* Broker sends PUBLISH on the data stream: initial, or DUP while awaiting PUBREC. *)
BrokerSendPublish ==
    /\ brokerState \in {"toSend", "awaitPubrec"}
    /\ sendCount < MaxSends
    /\ dataStream' = dataStream + 1
    /\ sendCount' = sendCount + 1
    /\ brokerState' = "awaitPubrec"
    /\ UNCHANGED <<dataStreamAck, ctrlToClient, ctrlToServer, pubrecSent, deliveryCount>>

(* Client receives a PUBLISH on the data stream. The dedup guard: deliver only *)
(* on first receipt; always answer with PUBREC, written to the SAME stream.    *)
ClientRecvPublish ==
    /\ dataStream > 0
    /\ dataStream' = dataStream - 1
    /\ deliveryCount' = IF pubrecSent
                        THEN deliveryCount
                        ELSE IF deliveryCount < 2 THEN deliveryCount + 1 ELSE deliveryCount
    /\ pubrecSent' = TRUE
    /\ dataStreamAck' = dataStreamAck + 1
    /\ UNCHANGED <<brokerState, sendCount, ctrlToClient, ctrlToServer>>

(* THE BUG. The broker only learns of the PUBREC if it reads the data stream's *)
(* recv half. With BrokerReadsDataStream = FALSE this action is disabled, the   *)
(* PUBREC is discarded, and the broker is stuck in "awaitPubrec" forever.       *)
BrokerRecvPubrec ==
    /\ BrokerReadsDataStream
    /\ dataStreamAck > 0
    /\ brokerState = "awaitPubrec"
    /\ dataStreamAck' = dataStreamAck - 1
    /\ brokerState' = "awaitPubcomp"
    /\ ctrlToClient' = ctrlToClient + 1
    /\ UNCHANGED <<sendCount, dataStream, ctrlToServer, pubrecSent, deliveryCount>>

(* A PUBREC written to a stream nobody reads is simply lost. Modelling this     *)
(* explicitly (rather than leaving it queued) keeps the wedge visible.          *)
PubrecDiscarded ==
    /\ ~BrokerReadsDataStream
    /\ dataStreamAck > 0
    /\ dataStreamAck' = dataStreamAck - 1
    /\ UNCHANGED <<brokerState, sendCount, dataStream, ctrlToClient, ctrlToServer,
                   pubrecSent, deliveryCount>>

(* Client receives PUBREL on the CONTROL stream -> clears the dedup marker.     *)
ClientRecvPubrel ==
    /\ ctrlToClient > 0
    /\ ctrlToClient' = ctrlToClient - 1
    /\ pubrecSent' = FALSE
    /\ ctrlToServer' = ctrlToServer + 1
    /\ UNCHANGED <<brokerState, sendCount, dataStream, dataStreamAck, deliveryCount>>

BrokerRecvPubcomp ==
    /\ ctrlToServer > 0
    /\ brokerState = "awaitPubcomp"
    /\ ctrlToServer' = ctrlToServer - 1
    /\ brokerState' = "done"
    /\ UNCHANGED <<sendCount, dataStream, dataStreamAck, ctrlToClient, pubrecSent,
                   deliveryCount>>

Done == brokerState = "done" /\ dataStream = 0 /\ UNCHANGED vars

Next ==
    \/ BrokerSendPublish
    \/ ClientRecvPublish
    \/ BrokerRecvPubrec
    \/ PubrecDiscarded
    \/ ClientRecvPubrel
    \/ BrokerRecvPubcomp
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ brokerState \in {"toSend", "awaitPubrec", "awaitPubcomp", "done"}
    /\ sendCount \in 0..MaxSends
    /\ dataStream \in 0..MaxSends
    /\ dataStreamAck \in 0..MaxSends
    /\ ctrlToClient \in 0..MaxSends
    /\ ctrlToServer \in 0..MaxSends
    /\ pubrecSent \in BOOLEAN
    /\ deliveryCount \in 0..2

(* #112 must still hold under the two-stream topology. *)
InvNoDuplicateDelivery == deliveryCount <= 1

(* The wedge. Once every PUBLISH transmission is spent and the client has been  *)
(* delivered the message, the broker must not still be waiting for a PUBREC it  *)
(* can never receive. Violated when the broker does not read the data stream.   *)
Wedged ==
    /\ sendCount = MaxSends
    /\ dataStream = 0
    /\ dataStreamAck = 0
    /\ deliveryCount > 0
    /\ brokerState = "awaitPubrec"

InvHandshakeCanComplete == ~Wedged
============================================================================
