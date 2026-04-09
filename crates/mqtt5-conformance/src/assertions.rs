//! Reusable assertions for conformance tests.
//!
//! Each helper takes a `[MQTT-x.x.x-y]` conformance ID string so the panic
//! message points directly at the spec statement that failed. The helpers
//! decode raw broker responses through [`packet_parser`](crate::packet_parser)
//! and compare against the test expectation.

#![allow(clippy::missing_panics_doc)]

use crate::packet_parser::{ParsedConnAck, ParsedProperties, ParsedPublish};
use crate::raw_client::RawMqttClient;
use crate::transport::Transport;
use std::time::Duration;

/// Reads a CONNACK and asserts the reason code matches `expected`.
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

/// Reads a PUBLISH and asserts the topic name equals `expected_topic`.
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
