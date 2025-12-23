use mqtt5_protocol::client::{ClientProtocol, ClientState, ProtocolAction, TimeoutId};
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::subscribe::SubscriptionOptions;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::types::{ConnectOptions, QoS};

fn main() {
    println!("Sans-io MQTT Client Protocol Example");
    println!("=====================================\n");

    let mut protocol = ClientProtocol::new("embedded-device-001");

    println!("1. Initiating connection...");
    let connect_options = ConnectOptions::new("embedded-device-001").with_clean_start(true);

    let actions = protocol.connect(&connect_options);
    process_actions(&actions, "Connect");

    println!("\n2. Simulating CONNACK received from broker...");
    let connack = ConnAckPacket::new(false, ReasonCode::Success)
        .with_receive_maximum(100)
        .with_topic_alias_maximum(10);

    let actions = protocol.handle_connack(&connack);
    process_actions(&actions, "CONNACK handling");

    assert!(protocol.is_connected());
    println!("   ✓ Client is now connected!");

    println!("\n3. Subscribing to topic...");
    let filters = vec![
        (
            "sensors/temperature".to_string(),
            SubscriptionOptions::default().with_qos(QoS::AtLeastOnce),
        ),
        (
            "sensors/humidity".to_string(),
            SubscriptionOptions::default().with_qos(QoS::AtMostOnce),
        ),
    ];
    let actions = protocol.subscribe(&filters);
    process_actions(&actions, "Subscribe");

    println!("\n4. Publishing a message...");
    let actions = protocol.publish("actuators/led", b"ON", QoS::AtLeastOnce, false);
    process_actions(&actions, "Publish QoS1");

    println!("\n5. Publishing QoS 0 message (fire and forget)...");
    let actions = protocol.publish("status/heartbeat", b"alive", QoS::AtMostOnce, false);
    process_actions(&actions, "Publish QoS0");

    println!("\n6. Simulating incoming PUBLISH from broker...");
    let incoming_publish = PublishPacket::new("sensors/temperature", b"25.5", QoS::AtMostOnce);
    let actions = protocol.handle_publish(&incoming_publish);
    process_actions(&actions, "Incoming PUBLISH");

    println!("\n7. Sending PING...");
    let actions = protocol.ping();
    process_actions(&actions, "PING");

    println!("\n8. Handling PINGRESP...");
    let actions = protocol.handle_pingresp();
    process_actions(&actions, "PINGRESP");

    println!("\n9. Simulating timeout...");
    let actions = protocol.handle_timeout(TimeoutId::SubAck(1));
    process_actions(&actions, "Timeout");

    println!("\n10. Disconnecting...");
    let actions = protocol.disconnect(ReasonCode::Success);
    process_actions(&actions, "Disconnect");

    assert!(!protocol.is_connected());
    println!("    ✓ Client disconnected!");

    println!("\n=====================================");
    println!("Sans-io Pattern Summary:");
    println!("- ClientProtocol returns actions, doesn't do I/O");
    println!("- Your transport layer interprets SendPacket actions");
    println!("- Your timer system handles ScheduleTimeout/CancelTimeout");
    println!("- Works on any runtime: tokio, embassy, bare-metal, etc.");
}

fn timeout_id_to_string(timeout_id: TimeoutId) -> String {
    match timeout_id {
        TimeoutId::ConnAck => "ConnAck".to_string(),
        TimeoutId::PingResp => "PingResp".to_string(),
        TimeoutId::PubAck(id) => format!("PubAck({id})"),
        TimeoutId::PubRec(id) => format!("PubRec({id})"),
        TimeoutId::PubRel(id) => format!("PubRel({id})"),
        TimeoutId::PubComp(id) => format!("PubComp({id})"),
        TimeoutId::SubAck(id) => format!("SubAck({id})"),
        TimeoutId::UnsubAck(id) => format!("UnsubAck({id})"),
    }
}

fn packet_type_name(packet: &Packet) -> &'static str {
    match packet {
        Packet::Connect(_) => "CONNECT",
        Packet::ConnAck(_) => "CONNACK",
        Packet::Publish(_) => "PUBLISH",
        Packet::PubAck(_) => "PUBACK",
        Packet::PubRec(_) => "PUBREC",
        Packet::PubRel(_) => "PUBREL",
        Packet::PubComp(_) => "PUBCOMP",
        Packet::Subscribe(_) => "SUBSCRIBE",
        Packet::SubAck(_) => "SUBACK",
        Packet::Unsubscribe(_) => "UNSUBSCRIBE",
        Packet::UnsubAck(_) => "UNSUBACK",
        Packet::PingReq => "PINGREQ",
        Packet::PingResp => "PINGRESP",
        Packet::Disconnect(_) => "DISCONNECT",
        Packet::Auth(_) => "AUTH",
    }
}

fn state_name(state: &ClientState) -> &'static str {
    match state {
        ClientState::Disconnected => "Disconnected",
        ClientState::Connecting => "Connecting",
        ClientState::AwaitingAuth { .. } => "AwaitingAuth",
        ClientState::Connected { session_present } => {
            if *session_present {
                "Connected(session resumed)"
            } else {
                "Connected(new session)"
            }
        }
        ClientState::Disconnecting => "Disconnecting",
    }
}

fn process_actions(actions: &[ProtocolAction], context: &str) {
    let len = actions.len();
    println!("   {context} generated {len} action(s):");
    for action in actions {
        print_action(action);
    }
}

fn print_action(action: &ProtocolAction) {
    match action {
        ProtocolAction::SendPacket(packet) => {
            let name = packet_type_name(packet);
            println!("     → SendPacket({name})");
        }
        ProtocolAction::DeliverMessage(msg) => {
            let topic = &msg.topic;
            let payload_len = msg.payload.len();
            println!("     → DeliverMessage(topic={topic}, payload={payload_len} bytes)");
        }
        ProtocolAction::StateTransition(state) => {
            let name = state_name(state);
            println!("     → StateTransition({name})");
        }
        ProtocolAction::ScheduleTimeout {
            timeout_id,
            duration_ms,
        } => {
            let id_name = timeout_id_to_string(*timeout_id);
            println!("     → ScheduleTimeout({id_name}, {duration_ms}ms)");
        }
        ProtocolAction::CancelTimeout { timeout_id } => {
            let id_name = timeout_id_to_string(*timeout_id);
            println!("     → CancelTimeout({id_name})");
        }
        ProtocolAction::TrackPendingAck {
            packet_id,
            ack_type,
        } => println!("     → TrackPendingAck(id={packet_id}, type={ack_type:?})"),
        ProtocolAction::RemovePendingAck {
            packet_id,
            ack_type,
        } => println!("     → RemovePendingAck(id={packet_id}, type={ack_type:?})"),
        ProtocolAction::UpdateServerLimits {
            receive_maximum,
            max_packet_size,
            topic_alias_maximum,
        } => println!("     → UpdateServerLimits(rx_max={receive_maximum}, pkt_size={max_packet_size}, alias_max={topic_alias_maximum})"),
        ProtocolAction::ScheduleKeepalive { interval_secs } => {
            println!("     → ScheduleKeepalive({interval_secs}s)");
        }
        ProtocolAction::ConnectionComplete {
            session_present,
            server_keep_alive,
        } => println!("     → ConnectionComplete(session_present={session_present}, keep_alive={server_keep_alive:?})"),
        ProtocolAction::SubscribeComplete {
            packet_id,
            granted_qos,
        } => println!("     → SubscribeComplete(id={packet_id}, qos={granted_qos:?})"),
        ProtocolAction::UnsubscribeComplete {
            packet_id,
            reason_codes,
        } => println!("     → UnsubscribeComplete(id={packet_id}, codes={reason_codes:?})"),
        ProtocolAction::PublishComplete {
            packet_id,
            reason_code,
        } => println!("     → PublishComplete(id={packet_id}, reason={reason_code:?})"),
        ProtocolAction::Error { code, message } => {
            println!("     → Error({code:?}, {message})");
        }
        ProtocolAction::Disconnect { reason } => {
            println!("     → Disconnect({reason:?})");
        }
    }
}
