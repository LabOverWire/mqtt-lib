use crate::packet::connect::ConnectPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::subscribe::{SubscribePacket, TopicFilter};
use crate::packet::MqttPacket;
use crate::protocol::v5::properties::Properties;
use crate::QoS;
use proptest::prelude::*;

/// Property test strategy for generating valid MQTT strings
fn mqtt_string_strategy() -> impl Strategy<Value = String> {
    // MQTT strings can't contain null bytes and have length limits
    prop::string::string_regex("[^\x00]{1,100}")
        .unwrap()
        .prop_filter("Non-empty string", |s| !s.is_empty())
}

/// Property test strategy for generating valid topic names
fn topic_name_strategy() -> impl Strategy<Value = String> {
    // Topic names can't contain wildcards or null bytes
    prop::string::string_regex("[^\x00+#]{1,50}")
        .unwrap()
        .prop_filter("Valid topic", |s| {
            !s.is_empty() && !s.contains('+') && !s.contains('#')
        })
}

/// Property test strategy for generating valid client IDs
fn client_id_strategy() -> impl Strategy<Value = String> {
    // Client IDs can be empty or contain most characters except null
    prop::string::string_regex("[^\x00]{0,23}").unwrap()
}

/// Property test strategy for generating `QoS` values
fn qos_strategy() -> impl Strategy<Value = QoS> {
    prop_oneof![
        Just(QoS::AtMostOnce),
        Just(QoS::AtLeastOnce),
        Just(QoS::ExactlyOnce),
    ]
}

/// Property test strategy for generating binary payload
fn payload_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..1000)
}

proptest! {
    /// Test that CONNECT packets can be encoded and decoded correctly
    #[test]
    fn test_connect_packet_roundtrip(
        client_id in client_id_strategy(),
        keep_alive in 0u16..65535,
        clean_start in any::<bool>(),
        credentials in prop::option::of((mqtt_string_strategy(), prop::option::of(prop::collection::vec(any::<u8>(), 0..100)))),
    ) {
        let (username, password) = match credentials {
            Some((user, pass)) => (Some(user), pass),
            None => (None, None),
        };
        let connect = ConnectPacket {
            client_id,
            keep_alive,
            clean_start,
            will: None, // Keep simple for now
            username,
            password,
            properties: Properties::new(),
            protocol_version: 5,
            will_properties: Properties::new(),
        };

        // Test encoding
        let mut buffer = Vec::new();
        let encode_result = connect.encode(&mut buffer);
        prop_assert!(encode_result.is_ok(), "Encoding should succeed: {:?}", encode_result);

        // Test decoding
        let mut buffer_bytes = bytes::Bytes::from(buffer);
        let fixed_header = crate::packet::FixedHeader::decode(&mut buffer_bytes).unwrap();
        let decode_result = ConnectPacket::decode_body(&mut buffer_bytes, &fixed_header);
        prop_assert!(decode_result.is_ok(), "Decoding should succeed: {:?}", decode_result);

        let decoded = decode_result.unwrap();

        // Verify roundtrip properties
        prop_assert_eq!(decoded.client_id, connect.client_id);
        prop_assert_eq!(decoded.keep_alive, connect.keep_alive);
        prop_assert_eq!(decoded.clean_start, connect.clean_start);
        prop_assert_eq!(decoded.username, connect.username);
        prop_assert_eq!(decoded.password, connect.password);
        prop_assert_eq!(decoded.protocol_version, connect.protocol_version);
    }

    /// Test that PUBLISH packets can be encoded and decoded correctly
    #[test]
    fn test_publish_packet_roundtrip(
        topic in topic_name_strategy(),
        payload in payload_strategy(),
        qos in qos_strategy(),
        retain in any::<bool>(),
        dup in any::<bool>(),
    ) {
        let packet_id = if matches!(qos, QoS::AtMostOnce) { None } else { Some(42u16) };

        let publish = PublishPacket {
            topic_name: topic,
            payload,
            qos,
            retain,
            dup,
            packet_id,
            properties: Properties::new(),
        };

        // Test encoding
        let mut buffer = Vec::new();
        let encode_result = publish.encode(&mut buffer);
        prop_assert!(encode_result.is_ok(), "Encoding should succeed: {:?}", encode_result);

        // Test decoding
        let mut buffer_bytes = bytes::Bytes::from(buffer);
        let fixed_header = crate::packet::FixedHeader::decode(&mut buffer_bytes).unwrap();
        let decode_result = PublishPacket::decode_body(&mut buffer_bytes, &fixed_header);
        prop_assert!(decode_result.is_ok(), "Decoding should succeed: {:?}", decode_result);

        let decoded = decode_result.unwrap();

        // Verify roundtrip properties
        prop_assert_eq!(decoded.topic_name, publish.topic_name);
        prop_assert_eq!(decoded.payload, publish.payload);
        prop_assert_eq!(decoded.qos, publish.qos);
        prop_assert_eq!(decoded.retain, publish.retain);
        prop_assert_eq!(decoded.dup, publish.dup);
        prop_assert_eq!(decoded.packet_id, publish.packet_id);
    }

    /// Test that SUBSCRIBE packets can be encoded and decoded correctly
    #[test]
    fn test_subscribe_packet_roundtrip(
        packet_id in 1u16..65535,
        topics in prop::collection::vec(
            (topic_name_strategy(), qos_strategy()),
            1..10
        ),
    ) {
        let filters: Vec<TopicFilter> = topics
            .into_iter()
            .map(|(topic, qos)| TopicFilter::new(topic, qos))
            .collect();

        let subscribe = SubscribePacket {
            packet_id,
            filters: filters.clone(),
            properties: Properties::new(),
        };

        // Test encoding
        let mut buffer = Vec::new();
        let encode_result = subscribe.encode(&mut buffer);
        prop_assert!(encode_result.is_ok(), "Encoding should succeed: {:?}", encode_result);

        // Test decoding
        let mut buffer_bytes = bytes::Bytes::from(buffer);
        let fixed_header = crate::packet::FixedHeader::decode(&mut buffer_bytes).unwrap();
        let decode_result = SubscribePacket::decode_body(&mut buffer_bytes, &fixed_header);
        prop_assert!(decode_result.is_ok(), "Decoding should succeed: {:?}", decode_result);

        let decoded = decode_result.unwrap();

        // Verify roundtrip properties
        prop_assert_eq!(decoded.packet_id, subscribe.packet_id);
        prop_assert_eq!(decoded.filters.len(), subscribe.filters.len());

        for (original, decoded_filter) in filters.iter().zip(decoded.filters.iter()) {
            prop_assert_eq!(&decoded_filter.filter, &original.filter);
            prop_assert_eq!(decoded_filter.options.qos, original.options.qos);
        }
    }

    /// Test variable length integer encoding/decoding properties
    #[test]
    fn test_variable_length_integer_roundtrip(value in 0u32..268_435_455) {
        let mut buffer = Vec::new();

        // Encode
        let encode_result = crate::encoding::encode_variable_int(&mut buffer, value);
        prop_assert!(encode_result.is_ok(), "Encoding should succeed for value {}: {:?}", value, encode_result);

        // Decode
        let mut buffer_bytes = bytes::Bytes::from(buffer);
        let decode_result = crate::encoding::decode_variable_int(&mut buffer_bytes);
        prop_assert!(decode_result.is_ok(), "Decoding should succeed for value {}: {:?}", value, decode_result);

        let decoded_value = decode_result.unwrap();
        prop_assert_eq!(decoded_value, value, "Roundtrip should preserve value");
    }

    /// Test string encoding/decoding properties
    #[test]
    fn test_string_encoding_roundtrip(
        input in prop::string::string_regex("[^\x00]{0,200}").unwrap()
    ) {
        use crate::encoding::{encode_string, decode_string};

        let mut buffer = Vec::new();

        // Encode
        let encode_result = encode_string(&mut buffer, &input);
        prop_assert!(encode_result.is_ok(), "String encoding should succeed: {:?}", encode_result);

        // Decode
        let mut buffer_bytes = bytes::Bytes::from(buffer);
        let decode_result = decode_string(&mut buffer_bytes);
        prop_assert!(decode_result.is_ok(), "String decoding should succeed: {:?}", decode_result);

        let decoded = decode_result.unwrap();
        prop_assert_eq!(decoded, input, "String roundtrip should preserve content");
    }

    /// Test binary data encoding/decoding properties
    #[test]
    fn test_binary_encoding_roundtrip(
        input in prop::collection::vec(any::<u8>(), 0..1000)
    ) {
        use crate::encoding::{encode_binary, decode_binary};

        let mut buffer = Vec::new();

        // Encode
        let encode_result = encode_binary(&mut buffer, &input);
        prop_assert!(encode_result.is_ok(), "Binary encoding should succeed: {:?}", encode_result);

        // Decode
        let mut buffer_bytes = bytes::Bytes::from(buffer);
        let decode_result = decode_binary(&mut buffer_bytes);
        prop_assert!(decode_result.is_ok(), "Binary decoding should succeed: {:?}", decode_result);

        let decoded = decode_result.unwrap();
        prop_assert_eq!(decoded, input, "Binary roundtrip should preserve content");
    }
}

#[cfg(test)]
mod property_invariant_tests {
    use super::*;

    proptest! {
        /// Test that encoded packet sizes are within reasonable bounds
        #[test]
        fn test_packet_size_bounds(
            client_id in client_id_strategy(),
            topic in topic_name_strategy(),
            payload in prop::collection::vec(any::<u8>(), 0..100),
        ) {
            let connect = ConnectPacket {
                client_id: client_id.clone(),
                keep_alive: 60,
                clean_start: true,
                will: None,
                username: None,
                password: None,
                properties: Properties::new(),
                protocol_version: 5,
                will_properties: Properties::new(),
            };

            let mut buffer = Vec::new();
            if connect.encode(&mut buffer).is_ok() {
                // Encoded packet should not be excessively large
                prop_assert!(buffer.len() < 1000, "CONNECT packet too large: {} bytes", buffer.len());

                // Should have minimum required bytes
                prop_assert!(buffer.len() >= 10, "CONNECT packet too small: {} bytes", buffer.len());
            }

            // Test publish packet size
            let publish = PublishPacket {
                topic_name: topic.clone(),
                payload: payload.clone(),
                qos: QoS::AtMostOnce,
                retain: false,
                dup: false,
                packet_id: None,
                properties: Properties::new(),
            };

            let mut pub_buffer = Vec::new();
            if publish.encode(&mut pub_buffer).is_ok() {
                // Should be roughly topic length + payload length + headers
                let expected_min = topic.len() + payload.len();
                let expected_max = expected_min + 100; // Allow for headers and encoding overhead

                prop_assert!(pub_buffer.len() >= expected_min,
                    "PUBLISH packet too small: {} bytes, expected at least {}",
                    pub_buffer.len(), expected_min);
                prop_assert!(pub_buffer.len() <= expected_max,
                    "PUBLISH packet too large: {} bytes, expected at most {}",
                    pub_buffer.len(), expected_max);
            }
        }

        /// Test that packet encoding is deterministic
        #[test]
        fn test_packet_encoding_deterministic(
            client_id in client_id_strategy(),
            keep_alive in 0u16..65535,
        ) {
            let connect = ConnectPacket {
                client_id,
                keep_alive,
                clean_start: true,
                will: None,
                username: None,
                password: None,
                properties: Properties::new(),
                protocol_version: 5,
                will_properties: Properties::new(),
            };

            // Encode multiple times
            let mut buffer1 = Vec::new();
            let mut buffer2 = Vec::new();

            let result1 = connect.encode(&mut buffer1);
            let result2 = connect.encode(&mut buffer2);

            if result1.is_ok() && result2.is_ok() {
                prop_assert_eq!(buffer1, buffer2, "Packet encoding should be deterministic");
            }
        }
    }
}
