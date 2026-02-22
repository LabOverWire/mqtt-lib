use mqtt5_conformance::harness::{unique_client_id, ConformanceBroker};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-3.1.3-11]` The Will Topic MUST be a UTF-8 Encoded String.
/// Sending invalid UTF-8 bytes in the Will Topic must cause disconnect.
#[tokio::test]
async fn will_topic_invalid_utf8_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("will-utf8");

    raw.send_raw(&RawPacketBuilder::connect_with_invalid_utf8_will_topic(
        &client_id,
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.3-11] server must reject CONNECT with invalid UTF-8 Will Topic"
    );
}

/// `[MQTT-3.1.3-12]` The User Name MUST be a UTF-8 Encoded String.
/// Sending invalid UTF-8 bytes in the Username must cause disconnect.
#[tokio::test]
async fn username_invalid_utf8_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("user-utf8");

    raw.send_raw(&RawPacketBuilder::connect_with_invalid_utf8_username(
        &client_id,
    ))
    .await
    .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.1.3-12] server must reject CONNECT with invalid UTF-8 Username"
    );
}

/// `[MQTT-3.14.0-1]` Server MUST NOT send DISCONNECT until after it has sent a
/// CONNACK with Reason Code < 0x80.
///
/// Send a CONNECT with an invalid protocol version — the broker should send
/// a CONNACK with error code, NOT a DISCONNECT first.
#[tokio::test]
async fn no_server_disconnect_before_connack() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_protocol_version(99))
        .await
        .unwrap();

    if let Some(bytes) = raw.read_packet_bytes(TIMEOUT).await {
        assert_ne!(
            bytes[0], 0xE0,
            "[MQTT-3.14.0-1] first packet from server must not be DISCONNECT (0xE0), got 0x{:02X}",
            bytes[0]
        );
        assert_eq!(
            bytes[0], 0x20,
            "expected CONNACK (0x20), got 0x{:02X}",
            bytes[0]
        );
    }
}

/// `[MQTT-3.14.2-2]` Session Expiry Interval MUST NOT be sent on a server DISCONNECT.
///
/// Trigger a server-initiated DISCONNECT (second CONNECT) and verify the
/// DISCONNECT packet properties do not contain Session Expiry Interval (0x11).
#[tokio::test]
async fn server_disconnect_no_session_expiry() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("disc-noexp");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let data = raw.expect_disconnect_raw(TIMEOUT).await;
    if let Some(bytes) = data {
        let mut idx = 1;
        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= bytes.len() {
                break;
            }
            let byte = bytes[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                break;
            }
        }
        if remaining_len >= 2 {
            idx += 1;
            let mut props_len: u32 = 0;
            let mut pshift = 0;
            loop {
                if idx >= bytes.len() {
                    break;
                }
                let byte = bytes[idx];
                idx += 1;
                props_len |= u32::from(byte & 0x7F) << pshift;
                if byte & 0x80 == 0 {
                    break;
                }
                pshift += 7;
                if pshift > 21 {
                    break;
                }
            }
            let props_end = idx + props_len as usize;
            while idx < props_end && idx < bytes.len() {
                let prop_id = bytes[idx];
                assert_ne!(
                    prop_id, 0x11,
                    "[MQTT-3.14.2-2] server DISCONNECT must not contain Session Expiry Interval (0x11)"
                );
                idx += 1;
                match prop_id {
                    0x1F => {
                        if idx + 2 <= bytes.len() {
                            let len = u16::from_be_bytes([bytes[idx], bytes[idx + 1]]) as usize;
                            idx += 2 + len;
                        } else {
                            break;
                        }
                    }
                    0x26 => {
                        if idx + 2 <= bytes.len() {
                            let klen = u16::from_be_bytes([bytes[idx], bytes[idx + 1]]) as usize;
                            idx += 2 + klen;
                            if idx + 2 <= bytes.len() {
                                let vlen =
                                    u16::from_be_bytes([bytes[idx], bytes[idx + 1]]) as usize;
                                idx += 2 + vlen;
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }
    }
}

/// `[MQTT-3.1.2-29]` If Request Problem Information is 0, the Server MUST NOT
/// send a Reason String or User Property on any packet other than PUBLISH,
/// CONNACK, or DISCONNECT.
///
/// Connect with `request_problem_info=0`, trigger a SUBACK with an error reason
/// code (subscribe to a topic matching an ACL deny pattern), and verify the
/// SUBACK contains no Reason String (`0x1F`) or User Property (`0x26`).
#[tokio::test]
async fn request_problem_info_zero_suppresses_properties() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("rpi-zero");

    raw.send_raw(&RawPacketBuilder::connect_with_request_problem_info(
        &client_id, 0,
    ))
    .await
    .unwrap();
    raw.expect_connack(TIMEOUT).await.expect("expected CONNACK");

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/rpi-zero",
        0,
        1,
    ))
    .await
    .unwrap();

    let data = raw
        .read_packet_bytes(TIMEOUT)
        .await
        .expect("expected SUBACK");
    assert_eq!(data[0], 0x90, "expected SUBACK packet type");

    let mut idx = 1;
    while idx < data.len() && (data[idx] & 0x80) != 0 {
        idx += 1;
    }
    idx += 1;
    idx += 2;
    let mut props_len: u32 = 0;
    let mut pshift = 0;
    loop {
        if idx >= data.len() {
            break;
        }
        let byte = data[idx];
        idx += 1;
        props_len |= u32::from(byte & 0x7F) << pshift;
        if byte & 0x80 == 0 {
            break;
        }
        pshift += 7;
    }
    let props_end = idx + props_len as usize;
    while idx < props_end && idx < data.len() {
        let prop_id = data[idx];
        assert_ne!(
            prop_id, 0x1F,
            "[MQTT-3.1.2-29] SUBACK must not contain Reason String when Request Problem Information=0"
        );
        assert_ne!(
            prop_id, 0x26,
            "[MQTT-3.1.2-29] SUBACK must not contain User Property when Request Problem Information=0"
        );
        idx += 1;
    }
}

/// `[MQTT-4.3.3-7]` Server MUST NOT apply message expiry if PUBLISH has been sent.
///
/// Subscribe at `QoS` 2, publish with Message Expiry Interval=1s, receive the
/// PUBLISH from the broker, wait >1s, then send PUBREC. The broker must
/// still respond with PUBREL (not drop the message).
#[tokio::test]
async fn qos2_no_message_expiry_after_publish_sent() {
    let broker = ConformanceBroker::start().await;
    let mut sub_raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("exp-sub");
    sub_raw.connect_and_establish(&sub_id, TIMEOUT).await;

    sub_raw
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(
            "test/expiry-sent",
            2,
            1,
        ))
        .await
        .unwrap();
    sub_raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut pub_raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("exp-pub");
    pub_raw.connect_and_establish(&pub_id, TIMEOUT).await;

    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos2_with_message_expiry(
            "test/expiry-sent",
            b"expiring-payload",
            10,
            1,
        ))
        .await
        .unwrap();
    pub_raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("expected PUBREC for publisher");
    pub_raw
        .send_raw(&RawPacketBuilder::pubrel(10))
        .await
        .unwrap();
    pub_raw
        .expect_pubcomp(TIMEOUT)
        .await
        .expect("expected PUBCOMP for publisher");

    let publish_pid = sub_raw
        .expect_publish_qos2(TIMEOUT)
        .await
        .expect("subscriber must receive QoS 2 PUBLISH");

    tokio::time::sleep(Duration::from_millis(1500)).await;

    sub_raw
        .send_raw(&RawPacketBuilder::pubrec(publish_pid))
        .await
        .unwrap();

    let pubrel = sub_raw.expect_pubrel_raw(TIMEOUT).await;
    assert!(
        pubrel.is_some(),
        "[MQTT-4.3.3-7] broker must send PUBREL even after message expiry elapsed — must not discard after PUBLISH sent"
    );

    let (_, pubrel_pid, _) = pubrel.unwrap();
    sub_raw
        .send_raw(&RawPacketBuilder::pubcomp(pubrel_pid))
        .await
        .unwrap();
}

/// `[MQTT-4.3.3-13]` Server MUST continue the `QoS` 2 acknowledgement sequence
/// even if it has applied message expiry.
///
/// Publisher sends `QoS` 2 PUBLISH with Message Expiry Interval=1s, broker
/// responds with PUBREC, publisher waits >1s, then sends PUBREL. Broker
/// must respond with PUBCOMP.
#[tokio::test]
async fn qos2_continues_despite_expiry() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("exp-cont");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_qos2_with_message_expiry(
        "test/expiry-continue",
        b"expiring",
        20,
        1,
    ))
    .await
    .unwrap();

    let pubrec = raw
        .expect_pubrec(TIMEOUT)
        .await
        .expect("expected PUBREC from broker");
    assert_eq!(pubrec.0, 20, "PUBREC packet ID must match");

    tokio::time::sleep(Duration::from_millis(1500)).await;

    raw.send_raw(&RawPacketBuilder::pubrel(20)).await.unwrap();

    let pubcomp = raw.expect_pubcomp(TIMEOUT).await;
    assert!(
        pubcomp.is_some(),
        "[MQTT-4.3.3-13] broker must send PUBCOMP even after message expiry — must continue QoS 2 sequence"
    );
    assert_eq!(pubcomp.unwrap().0, 20, "PUBCOMP packet ID must match");
}
