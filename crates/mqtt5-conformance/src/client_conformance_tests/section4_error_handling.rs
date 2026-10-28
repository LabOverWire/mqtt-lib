use super::common::inject_after_subscribe;
use super::section1_data_repr::surrogate_topic_publish;
use crate::client_conformance_test;
use crate::client_sut::ClientSutHandle;

/// [MQTT-1.5.4-1] [MQTT-4.13.2-1] After a Malformed Packet the Client closes
/// the Network Connection.
#[client_conformance_test(ids = ["MQTT-1.5.4-1", "MQTT-4.13.2-1"])]
async fn network_connection_closed_after_malformed_packet(sut: ClientSutHandle) {
    let outcome =
        inject_after_subscribe(&sut, "close-after-malformed", &surrogate_topic_publish()).await;
    assert!(
        outcome.close.closed,
        "[MQTT-4.13.2-1] after a Malformed Packet the client left the network connection open for 3s (is_connected={}, DISCONNECT reason {:?})",
        outcome.still_connected,
        outcome.close.disconnect_reason
    );
}
