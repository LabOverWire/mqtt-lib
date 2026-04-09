//! Typed views over raw MQTT v5.0 packet bytes.
//!
//! [`RawMqttClient`](crate::raw_client::RawMqttClient) hands out raw `Vec<u8>`
//! responses from the broker. Tests historically duplicated property-walking
//! logic inline; this module consolidates the variable-length integer decoding,
//! property-table walking, and per-packet field extraction into typed views
//! that borrow from the underlying byte buffer.
//!
//! All parsers are tolerant: a malformed or short buffer returns `None` rather
//! than panicking, so tests can express expectations as `expect("…")` with a
//! conformance-statement message attached.

#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

const PROP_PAYLOAD_FORMAT_INDICATOR: u8 = 0x01;
const PROP_MESSAGE_EXPIRY_INTERVAL: u8 = 0x02;
const PROP_CONTENT_TYPE: u8 = 0x03;
const PROP_RESPONSE_TOPIC: u8 = 0x08;
const PROP_CORRELATION_DATA: u8 = 0x09;
const PROP_SUBSCRIPTION_IDENTIFIER: u8 = 0x0B;
const PROP_TOPIC_ALIAS: u8 = 0x23;
const PROP_USER_PROPERTY: u8 = 0x26;

/// Decodes an MQTT variable-length integer starting at `start` in `data`.
///
/// Returns the decoded value and the index of the byte immediately following
/// the encoded integer, or `None` if the buffer is short or the integer is
/// malformed (more than 4 continuation bytes).
#[must_use]
pub fn decode_variable_int(data: &[u8], start: usize) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    let mut shift = 0;
    let mut idx = start;
    loop {
        if idx >= data.len() {
            return None;
        }
        let byte = data[idx];
        idx += 1;
        value |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some((value, idx));
        }
        shift += 7;
        if shift > 21 {
            return None;
        }
    }
}

/// Decodes a length-prefixed UTF-8 string starting at `idx`.
///
/// Returns the decoded string and the index of the byte immediately following
/// the string, or `None` on truncation or invalid UTF-8.
#[must_use]
pub fn decode_string(data: &[u8], idx: usize) -> Option<(String, usize)> {
    if idx + 2 > data.len() {
        return None;
    }
    let len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
    let start = idx + 2;
    let end = start + len;
    if end > data.len() {
        return None;
    }
    let s = String::from_utf8(data[start..end].to_vec()).ok()?;
    Some((s, end))
}

/// A typed view over the property table of any MQTT v5.0 packet.
///
/// Walks the variable-length property block once on construction and exposes
/// accessors for the most common fields. Unknown property IDs are tolerated
/// (the walker advances by the correct length for every defined property and
/// returns `None` for unrecognized IDs encountered during eager extraction).
#[derive(Debug, Clone, Default)]
pub struct ParsedProperties {
    pub user_properties: Vec<(String, String)>,
    pub subscription_identifiers: Vec<u32>,
    pub topic_alias: Option<u16>,
    pub content_type: Option<String>,
    pub response_topic: Option<String>,
    pub correlation_data: Option<Vec<u8>>,
    pub payload_format_indicator: Option<u8>,
    pub message_expiry_interval: Option<u32>,
}

impl ParsedProperties {
    /// Parses the property block at `[start..start+len]`.
    ///
    /// Returns `None` if any property's body is truncated or unknown.
    #[must_use]
    pub fn parse(data: &[u8], start: usize, len: usize) -> Option<Self> {
        let end = start + len;
        if end > data.len() {
            return None;
        }
        let mut props = Self::default();
        let mut idx = start;
        while idx < end {
            let prop_id = data[idx];
            idx += 1;
            match prop_id {
                PROP_PAYLOAD_FORMAT_INDICATOR => {
                    if idx >= end {
                        return None;
                    }
                    props.payload_format_indicator = Some(data[idx]);
                    idx += 1;
                }
                PROP_MESSAGE_EXPIRY_INTERVAL => {
                    if idx + 4 > end {
                        return None;
                    }
                    props.message_expiry_interval = Some(u32::from_be_bytes([
                        data[idx],
                        data[idx + 1],
                        data[idx + 2],
                        data[idx + 3],
                    ]));
                    idx += 4;
                }
                PROP_CONTENT_TYPE => {
                    let (s, next) = decode_string(data, idx)?;
                    props.content_type = Some(s);
                    idx = next;
                }
                PROP_RESPONSE_TOPIC => {
                    let (s, next) = decode_string(data, idx)?;
                    props.response_topic = Some(s);
                    idx = next;
                }
                PROP_CORRELATION_DATA => {
                    if idx + 2 > end {
                        return None;
                    }
                    let len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
                    let body_start = idx + 2;
                    let body_end = body_start + len;
                    if body_end > end {
                        return None;
                    }
                    props.correlation_data = Some(data[body_start..body_end].to_vec());
                    idx = body_end;
                }
                PROP_SUBSCRIPTION_IDENTIFIER => {
                    let (val, next) = decode_variable_int(data, idx)?;
                    props.subscription_identifiers.push(val);
                    idx = next;
                }
                PROP_TOPIC_ALIAS => {
                    if idx + 2 > end {
                        return None;
                    }
                    props.topic_alias = Some(u16::from_be_bytes([data[idx], data[idx + 1]]));
                    idx += 2;
                }
                PROP_USER_PROPERTY => {
                    let (key, after_key) = decode_string(data, idx)?;
                    let (value, after_value) = decode_string(data, after_key)?;
                    props.user_properties.push((key, value));
                    idx = after_value;
                }
                _ => return None,
            }
        }
        Some(props)
    }
}

/// A typed view over a PUBLISH packet's bytes.
#[derive(Debug, Clone)]
pub struct ParsedPublish {
    pub dup: bool,
    pub qos: u8,
    pub retain: bool,
    pub topic: String,
    pub packet_id: Option<u16>,
    pub properties: ParsedProperties,
    pub payload: Vec<u8>,
}

impl ParsedPublish {
    /// Parses a PUBLISH packet from raw bytes.
    ///
    /// Returns `None` if the first byte is not a PUBLISH (`0x3X`) or any field
    /// is truncated.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.is_empty() || (data[0] & 0xF0) != 0x30 {
            return None;
        }
        let first_byte = data[0];
        let dup = (first_byte & 0x08) != 0;
        let qos = (first_byte >> 1) & 0x03;
        let retain = (first_byte & 0x01) != 0;
        let (remaining_len, body_start) = decode_variable_int(data, 1)?;
        let body_end = body_start + remaining_len as usize;
        if body_end > data.len() {
            return None;
        }
        let mut idx = body_start;
        let (topic, after_topic) = decode_string(data, idx)?;
        idx = after_topic;
        let packet_id = if qos > 0 {
            if idx + 2 > body_end {
                return None;
            }
            let pid = u16::from_be_bytes([data[idx], data[idx + 1]]);
            idx += 2;
            Some(pid)
        } else {
            None
        };
        let (props_len, props_start) = decode_variable_int(data, idx)?;
        let properties = ParsedProperties::parse(data, props_start, props_len as usize)?;
        let payload_start = props_start + props_len as usize;
        if payload_start > body_end {
            return None;
        }
        let payload = data[payload_start..body_end].to_vec();
        Some(Self {
            dup,
            qos,
            retain,
            topic,
            packet_id,
            properties,
            payload,
        })
    }
}

/// A typed view over a CONNACK packet's bytes.
#[derive(Debug, Clone)]
pub struct ParsedConnAck {
    pub session_present: bool,
    pub reason_code: u8,
    pub properties: ParsedProperties,
}

impl ParsedConnAck {
    /// Parses a CONNACK packet from raw bytes.
    ///
    /// Returns `None` if the first byte is not `0x20` or any field is truncated.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.is_empty() || data[0] != 0x20 {
            return None;
        }
        let (remaining_len, body_start) = decode_variable_int(data, 1)?;
        let body_end = body_start + remaining_len as usize;
        if body_end > data.len() || remaining_len < 2 {
            return None;
        }
        let session_present = (data[body_start] & 0x01) != 0;
        let reason_code = data[body_start + 1];
        let after_fixed = body_start + 2;
        let properties = if after_fixed < body_end {
            let (props_len, props_start) = decode_variable_int(data, after_fixed)?;
            ParsedProperties::parse(data, props_start, props_len as usize)?
        } else {
            ParsedProperties::default()
        };
        Some(Self {
            session_present,
            reason_code,
            properties,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_int_decodes_single_byte() {
        let data = [0x05];
        assert_eq!(decode_variable_int(&data, 0), Some((5, 1)));
    }

    #[test]
    fn variable_int_decodes_multi_byte() {
        let data = [0x80, 0x01];
        assert_eq!(decode_variable_int(&data, 0), Some((128, 2)));
    }

    #[test]
    fn variable_int_rejects_overlong() {
        let data = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        assert_eq!(decode_variable_int(&data, 0), None);
    }

    #[test]
    fn properties_parse_user_properties() {
        let data = [
            0x26, 0x00, 0x01, b'k', 0x00, 0x01, b'v', 0x26, 0x00, 0x01, b'a', 0x00, 0x01, b'b',
        ];
        let props = ParsedProperties::parse(&data, 0, data.len()).unwrap();
        assert_eq!(
            props.user_properties,
            vec![
                ("k".to_owned(), "v".to_owned()),
                ("a".to_owned(), "b".to_owned()),
            ]
        );
    }

    #[test]
    fn properties_parse_multiple_subscription_identifiers() {
        let data = [0x0B, 0x05, 0x0B, 0x07];
        let props = ParsedProperties::parse(&data, 0, data.len()).unwrap();
        assert_eq!(props.subscription_identifiers, vec![5, 7]);
    }

    #[test]
    fn parsed_publish_extracts_qos1_with_properties() {
        let data = [
            0x32, 0x10, 0x00, 0x04, b't', b'/', b'a', b'b', 0x00, 0x01, 0x02, 0x01, 0x01, b'h',
            b'e', b'l', b'l', b'o',
        ];
        let pkt = ParsedPublish::parse(&data).unwrap();
        assert!(!pkt.dup);
        assert_eq!(pkt.qos, 1);
        assert!(!pkt.retain);
        assert_eq!(pkt.topic, "t/ab");
        assert_eq!(pkt.packet_id, Some(1));
        assert_eq!(pkt.properties.payload_format_indicator, Some(1));
        assert_eq!(pkt.payload, b"hello");
    }
}
