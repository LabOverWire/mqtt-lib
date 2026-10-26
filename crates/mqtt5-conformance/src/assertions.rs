//! Reusable assertions for conformance tests.
//!
//! Most helpers take a conformance ID such as `MQTT-x.x.x-y` and bracket it in
//! the panic message, so the message points at the spec statement that failed.
//! [`expect_no_publish`] takes the message context as given instead, since a
//! negative check may have no numbered statement; callers that cite one write
//! it in brackets themselves. The helpers decode raw broker responses through
//! [`packet_parser`](crate::packet_parser) and compare against the test
//! expectation.

use crate::packet_parser::{ParsedConnAck, ParsedProperties, ParsedPublish};
use crate::raw_client::{unique_client_id, RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::transport::Transport;
use std::time::Duration;

/// Reads a CONNACK and asserts the reason code matches `expected`.
///
/// # Panics
/// Panics if no valid CONNACK arrives or its reason code differs.
pub async fn expect_connack_reason<T: Transport>(
    raw: &mut RawMqttClient<T>,
    expected: u8,
    statement_id: &str,
    timeout: Duration,
) -> ParsedConnAck {
    let bytes = raw
        .read_packet_bytes(timeout)
        .await
        .unwrap_or_else(|| panic!("[{statement_id}] expected CONNACK, got nothing"));
    let connack = ParsedConnAck::parse(&bytes)
        .unwrap_or_else(|| panic!("[{statement_id}] expected valid CONNACK, got {bytes:?}"));
    assert_eq!(
        connack.reason_code, expected,
        "[{statement_id}] expected CONNACK reason 0x{expected:02X}, got 0x{:02X}",
        connack.reason_code
    );
    connack
}

/// Reads a SUBACK and asserts every reason code matches `expected_codes`.
///
/// # Panics
/// Panics if no SUBACK arrives or its reason codes differ.
pub async fn expect_suback_codes<T: Transport>(
    raw: &mut RawMqttClient<T>,
    expected_codes: &[u8],
    statement_id: &str,
    timeout: Duration,
) -> u16 {
    let (packet_id, codes) = raw
        .expect_suback(timeout)
        .await
        .unwrap_or_else(|| panic!("[{statement_id}] expected SUBACK"));
    assert_eq!(
        codes, expected_codes,
        "[{statement_id}] expected SUBACK reason codes {expected_codes:?}, got {codes:?}"
    );
    packet_id
}

/// Asserts the broker closes the connection or sends DISCONNECT within the timeout.
///
/// # Panics
/// Panics if the connection is still open when the timeout elapses.
pub async fn expect_disconnect<T: Transport>(
    raw: &mut RawMqttClient<T>,
    statement_id: &str,
    timeout: Duration,
) {
    assert!(
        raw.expect_disconnect(timeout).await,
        "[{statement_id}] server must disconnect"
    );
}

/// Connects a raw client and subscribes it to `filter` at `QoS` 0.
///
/// # Panics
/// Panics if the connection, the SUBSCRIBE or the SUBACK fails, or the
/// subscription is not granted at `QoS` 0.
pub async fn subscribed_raw_client(
    sut: &SutHandle,
    prefix: &str,
    filter: &str,
    timeout: Duration,
) -> RawMqttClient {
    let mut client = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .expect("TCP connect");
    client
        .connect_and_establish(&unique_client_id(prefix), timeout)
        .await;
    client
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(filter, 0, 1))
        .await
        .expect("send SUBSCRIBE");
    let (_, granted) = client
        .expect_suback(timeout)
        .await
        .expect("expected SUBACK");
    assert_eq!(
        granted,
        [0x00],
        "subscription to {filter} must be granted at QoS 0"
    );
    client
}

/// Reads packets until a PUBLISH whose topic `matches` arrives, returning its
/// topic, or `None` once `window` elapses or the connection ends.
pub async fn first_publish_matching<T: Transport>(
    raw: &mut RawMqttClient<T>,
    window: Duration,
    matches: impl Fn(&str) -> bool,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let packet = raw.read_mqtt_packet(remaining).await?;
        if let Some(publish) = ParsedPublish::parse(&packet) {
            if matches(&publish.topic) {
                return Some(publish.topic);
            }
        }
    }
}

/// Asserts that no PUBLISH whose topic `matches` arrives within `window`, and
/// that the connection is still open afterwards: a PINGREQ must get its
/// PINGRESP, so a closed connection does not pass as silence.
///
/// # Panics
/// Panics on a matching PUBLISH, on any packet other than PUBLISH or PINGRESP,
/// or if the connection ends before the PINGRESP.
pub async fn expect_no_publish<T: Transport>(
    raw: &mut RawMqttClient<T>,
    context: &str,
    window: Duration,
    matches: impl Fn(&str) -> bool,
) {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Some(packet) = raw.read_mqtt_packet(remaining).await else {
            break;
        };
        reject_unexpected(&packet, context, &matches);
    }
    raw.send_raw(&RawPacketBuilder::pingreq())
        .await
        .unwrap_or_else(|error| panic!("{context}: the connection must stay open: {error}"));
    loop {
        let packet = raw.read_mqtt_packet(window).await.unwrap_or_else(|| {
            panic!("{context}: the connection must stay open and answer PINGREQ")
        });
        if packet.first() == Some(&0xD0) {
            return;
        }
        reject_unexpected(&packet, context, &matches);
    }
}

fn reject_unexpected(packet: &[u8], context: &str, matches: &impl Fn(&str) -> bool) {
    let publish = ParsedPublish::parse(packet)
        .unwrap_or_else(|| panic!("{context}: expected no packet, got {packet:02x?}"));
    assert!(
        !matches(&publish.topic),
        "{context}: expected no PUBLISH on {}, got one",
        publish.topic
    );
}

/// Reads a PUBLISH and asserts the topic name equals `expected_topic`.
///
/// # Panics
/// Panics if no valid PUBLISH arrives or its topic differs.
pub async fn expect_publish_with_topic<T: Transport>(
    raw: &mut RawMqttClient<T>,
    expected_topic: &str,
    statement_id: &str,
    timeout: Duration,
) -> ParsedPublish {
    let bytes = raw
        .read_packet_bytes(timeout)
        .await
        .unwrap_or_else(|| panic!("[{statement_id}] expected PUBLISH, got nothing"));
    let publish = ParsedPublish::parse(&bytes)
        .unwrap_or_else(|| panic!("[{statement_id}] expected valid PUBLISH, got {bytes:?}"));
    assert_eq!(
        publish.topic, expected_topic,
        "[{statement_id}] expected topic '{expected_topic}', got '{}'",
        publish.topic
    );
    publish
}

/// Asserts that `actual` user properties contain every entry in `expected`,
/// allowing additional properties whose keys appear in `injected_keys`.
///
/// This encodes the broker-injected user property accommodation: brokers like
/// mqtt-lib inject `x-mqtt-sender` / `x-mqtt-client-id` on every PUBLISH, and
/// the conformance suite must tolerate them when comparing forwarded
/// application-level user properties.
///
/// # Panics
/// Panics if the non-injected user properties differ from `expected`.
pub fn expect_user_properties_subset(
    actual: &[(String, String)],
    expected: &[(String, String)],
    injected_keys: &[&str],
    statement_id: &str,
) {
    let filtered: Vec<(String, String)> = actual
        .iter()
        .filter(|(k, _)| !injected_keys.contains(&k.as_str()))
        .cloned()
        .collect();
    assert_eq!(
        filtered, expected,
        "[{statement_id}] user properties (excluding injected {injected_keys:?}) mismatch"
    );
}

/// Returns `true` if `props` contains a `(key, value)` pair matching `expected`.
#[must_use]
pub fn user_properties_contain(
    props: &ParsedProperties,
    expected_key: &str,
    expected_value: &str,
) -> bool {
    props
        .user_properties
        .iter()
        .any(|(k, v)| k == expected_key && v == expected_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_properties_subset_filters_injected() {
        let actual = vec![
            ("x-mqtt-sender".to_owned(), "alice".to_owned()),
            ("k".to_owned(), "v".to_owned()),
            ("x-mqtt-client-id".to_owned(), "client-1".to_owned()),
        ];
        let expected = vec![("k".to_owned(), "v".to_owned())];
        expect_user_properties_subset(
            &actual,
            &expected,
            &["x-mqtt-sender", "x-mqtt-client-id"],
            "MQTT-3.3.2-17",
        );
    }

    #[test]
    #[should_panic(expected = "MQTT-3.3.2-17")]
    fn user_properties_subset_panics_on_mismatch() {
        let actual = vec![("k".to_owned(), "wrong".to_owned())];
        let expected = vec![("k".to_owned(), "v".to_owned())];
        expect_user_properties_subset(&actual, &expected, &[], "MQTT-3.3.2-17");
    }
}
