use crate::encoding::{decode_string, encode_string};
use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::Properties;
use crate::QoS;
use bebytes::BeBytes;
use bytes::{Buf, BufMut};

/// Subscription options (v5.0)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionOptions {
    /// Maximum `QoS` level the client will accept
    pub qos: QoS,
    /// No Local option - if true, Application Messages MUST NOT be forwarded to this connection
    pub no_local: bool,
    /// Retain As Published - if true, keep the RETAIN flag as published
    pub retain_as_published: bool,
    /// Retain Handling option
    pub retain_handling: RetainHandling,
}

/// Retain handling options
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RetainHandling {
    /// Send retained messages at subscribe time
    SendAtSubscribe = 0,
    /// Send retained messages at subscribe time only if subscription doesn't exist
    SendAtSubscribeIfNew = 1,
    /// Don't send retained messages at subscribe time
    DoNotSend = 2,
}

/// Subscription options using bebytes for bit field operations
/// This demonstrates the hybrid approach for complex packet variable headers
/// Bit fields are ordered from MSB to LSB (bits 7-0)
#[derive(Debug, Clone, Copy, PartialEq, Eq, BeBytes)]
pub struct SubscriptionOptionsBits {
    /// Reserved bits (bits 7-6) - must be 0
    #[bits(2)]
    pub reserved_bits: u8,
    /// Retain Handling (bits 5-4)
    #[bits(2)]
    pub retain_handling: u8,
    /// Retain As Published flag (bit 3)
    #[bits(1)]
    pub retain_as_published: u8,
    /// No Local flag (bit 2)
    #[bits(1)]
    pub no_local: u8,
    /// `QoS` level (bits 1-0)
    #[bits(2)]
    pub qos: u8,
}

impl SubscriptionOptionsBits {
    /// Creates subscription options bits from high-level `SubscriptionOptions`
    /// Bebytes handles bit field layout, Rust handles type safety and validation
    #[must_use]
    pub fn from_options(options: &SubscriptionOptions) -> Self {
        Self {
            reserved_bits: 0,
            retain_handling: options.retain_handling as u8,
            retain_as_published: u8::from(options.retain_as_published),
            no_local: u8::from(options.no_local),
            qos: options.qos as u8,
        }
    }

    /// Converts bebytes bit fields back to high-level `SubscriptionOptions`
    /// Bebytes provides the bits, Rust handles validation and type conversion
    ///
    /// # Errors
    ///
    /// Returns an error if reserved bits are set, or if `QoS` or retain handling values are invalid
    pub fn to_options(&self) -> Result<SubscriptionOptions> {
        // Validate reserved bits are zero
        if self.reserved_bits != 0 {
            return Err(MqttError::MalformedPacket(
                "Reserved bits in subscription options must be 0".to_string(),
            ));
        }

        // Validate and convert QoS
        let qos = match self.qos {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => {
                return Err(MqttError::MalformedPacket(format!(
                    "Invalid QoS value in subscription options: {}",
                    self.qos
                )))
            }
        };

        // Validate and convert retain handling
        let retain_handling = match self.retain_handling {
            0 => RetainHandling::SendAtSubscribe,
            1 => RetainHandling::SendAtSubscribeIfNew,
            2 => RetainHandling::DoNotSend,
            _ => {
                return Err(MqttError::MalformedPacket(format!(
                    "Invalid retain handling value: {}",
                    self.retain_handling
                )))
            }
        };

        Ok(SubscriptionOptions {
            qos,
            no_local: self.no_local != 0,
            retain_as_published: self.retain_as_published != 0,
            retain_handling,
        })
    }
}

impl Default for SubscriptionOptions {
    fn default() -> Self {
        Self {
            qos: QoS::AtMostOnce,
            no_local: false,
            retain_as_published: false,
            retain_handling: RetainHandling::SendAtSubscribe,
        }
    }
}

impl SubscriptionOptions {
    /// Creates subscription options with the specified `QoS`
    #[must_use]
    pub fn new(qos: QoS) -> Self {
        Self {
            qos,
            ..Default::default()
        }
    }

    /// Sets the `QoS` level
    #[must_use]
    pub fn with_qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    /// Encodes subscription options as a byte (v5.0)
    /// Original manual implementation for comparison
    #[must_use]
    pub fn encode(&self) -> u8 {
        let mut byte = self.qos as u8;

        if self.no_local {
            byte |= 0x04;
        }

        if self.retain_as_published {
            byte |= 0x08;
        }

        byte |= (self.retain_handling as u8) << 4;

        byte
    }

    /// Encodes subscription options using bebytes (hybrid approach)
    /// Bebytes handles bit field operations, Rust handles type safety
    #[must_use]
    pub fn encode_with_bebytes(&self) -> u8 {
        let bits = SubscriptionOptionsBits::from_options(self);
        bits.to_be_bytes()[0]
    }

    /// Decodes subscription options from a byte (v5.0)
    /// Original manual implementation for comparison
    ///
    /// # Errors
    ///
    /// Returns an error if the `QoS` value is invalid
    pub fn decode(byte: u8) -> Result<Self> {
        let qos_val = byte & crate::constants::subscription::QOS_MASK;
        let qos = match qos_val {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => {
                return Err(MqttError::MalformedPacket(format!(
                    "Invalid QoS value in subscription options: {qos_val}"
                )))
            }
        };

        let no_local = (byte & crate::constants::subscription::NO_LOCAL_MASK) != 0;
        let retain_as_published =
            (byte & crate::constants::subscription::RETAIN_AS_PUBLISHED_MASK) != 0;

        let retain_handling_val = (byte >> crate::constants::subscription::RETAIN_HANDLING_SHIFT)
            & crate::constants::subscription::QOS_MASK;
        let retain_handling = match retain_handling_val {
            0 => RetainHandling::SendAtSubscribe,
            1 => RetainHandling::SendAtSubscribeIfNew,
            2 => RetainHandling::DoNotSend,
            _ => {
                return Err(MqttError::MalformedPacket(format!(
                    "Invalid retain handling value: {retain_handling_val}"
                )))
            }
        };

        // Check reserved bits
        if (byte & crate::constants::subscription::RESERVED_BITS_MASK) != 0 {
            return Err(MqttError::MalformedPacket(
                "Reserved bits in subscription options must be 0".to_string(),
            ));
        }

        Ok(Self {
            qos,
            no_local,
            retain_as_published,
            retain_handling,
        })
    }

    /// Decodes subscription options using bebytes (hybrid approach)\
    /// Bebytes handles bit field extraction, Rust handles validation and type conversion
    ///
    /// # Errors
    ///
    /// Returns an error if the `QoS` value or retain handling is invalid, or reserved bits are set
    pub fn decode_with_bebytes(byte: u8) -> Result<Self> {
        let (bits, _consumed) =
            SubscriptionOptionsBits::try_from_be_bytes(&[byte]).map_err(|e| {
                MqttError::MalformedPacket(format!("Invalid subscription options byte: {e}"))
            })?;

        bits.to_options()
    }
}

/// Topic filter with subscription options
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicFilter {
    /// Topic filter string (may contain wildcards)
    pub filter: String,
    /// Subscription options
    pub options: SubscriptionOptions,
}

impl TopicFilter {
    /// Creates a new topic filter with the specified `QoS`
    #[must_use]
    pub fn new(filter: impl Into<String>, qos: QoS) -> Self {
        Self {
            filter: filter.into(),
            options: SubscriptionOptions::new(qos),
        }
    }

    /// Creates a new topic filter with custom options
    #[must_use]
    pub fn with_options(filter: impl Into<String>, options: SubscriptionOptions) -> Self {
        Self {
            filter: filter.into(),
            options,
        }
    }
}

/// MQTT SUBSCRIBE packet
#[derive(Debug, Clone)]
pub struct SubscribePacket {
    /// Packet identifier
    pub packet_id: u16,
    /// Topic filters to subscribe to
    pub filters: Vec<TopicFilter>,
    /// SUBSCRIBE properties (v5.0 only)
    pub properties: Properties,
}

impl SubscribePacket {
    /// Creates a new SUBSCRIBE packet
    #[must_use]
    pub fn new(packet_id: u16) -> Self {
        Self {
            packet_id,
            filters: Vec::new(),
            properties: Properties::default(),
        }
    }

    /// Adds a topic filter
    #[must_use]
    pub fn add_filter(mut self, filter: impl Into<String>, qos: QoS) -> Self {
        self.filters.push(TopicFilter::new(filter, qos));
        self
    }

    /// Adds a topic filter with options
    #[must_use]
    pub fn add_filter_with_options(mut self, filter: TopicFilter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Sets the subscription identifier
    #[must_use]
    pub fn with_subscription_identifier(mut self, id: u32) -> Self {
        self.properties.set_subscription_identifier(id);
        self
    }

    /// Adds a user property
    #[must_use]
    pub fn with_user_property(mut self, key: String, value: String) -> Self {
        self.properties.add_user_property(key, value);
        self
    }
}

impl MqttPacket for SubscribePacket {
    fn packet_type(&self) -> PacketType {
        PacketType::Subscribe
    }

    fn flags(&self) -> u8 {
        0x02 // SUBSCRIBE must have flags = 0x02
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // Variable header
        buf.put_u16(self.packet_id);

        // Properties (v5.0)
        self.properties.encode(buf)?;

        // Payload - topic filters
        if self.filters.is_empty() {
            return Err(MqttError::MalformedPacket(
                "SUBSCRIBE packet must contain at least one topic filter".to_string(),
            ));
        }

        for filter in &self.filters {
            encode_string(buf, &filter.filter)?;
            buf.put_u8(filter.options.encode());
        }

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, fixed_header: &FixedHeader) -> Result<Self> {
        // Validate flags
        if fixed_header.flags != 0x02 {
            return Err(MqttError::MalformedPacket(format!(
                "Invalid SUBSCRIBE flags: expected 0x02, got 0x{:02X}",
                fixed_header.flags
            )));
        }

        // Packet identifier
        if buf.remaining() < 2 {
            return Err(MqttError::MalformedPacket(
                "SUBSCRIBE missing packet identifier".to_string(),
            ));
        }
        let packet_id = buf.get_u16();

        // Properties (v5.0)
        let properties = Properties::decode(buf)?;

        // Payload - topic filters
        let mut filters = Vec::new();

        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "SUBSCRIBE packet must contain at least one topic filter".to_string(),
            ));
        }

        while buf.has_remaining() {
            let filter_str = decode_string(buf)?;

            if !buf.has_remaining() {
                return Err(MqttError::MalformedPacket(
                    "Missing subscription options for topic filter".to_string(),
                ));
            }

            let options_byte = buf.get_u8();
            let options = SubscriptionOptions::decode(options_byte)?;

            filters.push(TopicFilter {
                filter: filter_str,
                options,
            });
        }

        Ok(Self {
            packet_id,
            filters,
            properties,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::v5::properties::PropertyId;
    use bebytes::BeBytes;
    use bytes::BytesMut;

    #[cfg(test)]
    mod hybrid_approach_tests {
        use super::*;
        use proptest::prelude::*;

        #[test]
        fn test_bebytes_vs_manual_encoding_identical() {
            // Test that bebytes and manual implementations produce identical results
            let test_cases = vec![
                SubscriptionOptions::default(),
                SubscriptionOptions {
                    qos: QoS::AtLeastOnce,
                    no_local: true,
                    retain_as_published: true,
                    retain_handling: RetainHandling::SendAtSubscribeIfNew,
                },
                SubscriptionOptions {
                    qos: QoS::ExactlyOnce,
                    no_local: false,
                    retain_as_published: true,
                    retain_handling: RetainHandling::DoNotSend,
                },
            ];

            for options in test_cases {
                let manual_encoded = options.encode();
                let bebytes_encoded = options.encode_with_bebytes();

                assert_eq!(
                    manual_encoded, bebytes_encoded,
                    "Manual and bebytes encoding should be identical for options: {options:?}"
                );

                // Also verify decoding produces same results
                let manual_decoded = SubscriptionOptions::decode(manual_encoded).unwrap();
                let bebytes_decoded =
                    SubscriptionOptions::decode_with_bebytes(bebytes_encoded).unwrap();

                assert_eq!(manual_decoded, bebytes_decoded);
                assert_eq!(manual_decoded, options);
            }
        }

        #[test]
        fn test_subscription_options_bits_round_trip() {
            let options = SubscriptionOptions {
                qos: QoS::AtLeastOnce,
                no_local: true,
                retain_as_published: false,
                retain_handling: RetainHandling::SendAtSubscribeIfNew,
            };

            let bits = SubscriptionOptionsBits::from_options(&options);
            let bytes = bits.to_be_bytes();
            assert_eq!(bytes.len(), 1);

            let (decoded_bits, consumed) =
                SubscriptionOptionsBits::try_from_be_bytes(&bytes).unwrap();
            assert_eq!(consumed, 1);
            assert_eq!(decoded_bits, bits);

            let decoded_options = decoded_bits.to_options().unwrap();
            assert_eq!(decoded_options, options);
        }

        #[test]
        fn test_reserved_bits_validation() {
            // Test that reserved bits being set causes validation errors
            let mut bits = SubscriptionOptionsBits::from_options(&SubscriptionOptions::default());

            // Set reserved bits
            bits.reserved_bits = 1;
            assert!(bits.to_options().is_err());

            // Set different reserved bit pattern
            bits.reserved_bits = 2;
            assert!(bits.to_options().is_err());
        }

        #[test]
        fn test_invalid_qos_validation() {
            let mut bits = SubscriptionOptionsBits::from_options(&SubscriptionOptions::default());
            bits.qos = 3; // Invalid QoS
            assert!(bits.to_options().is_err());
        }

        #[test]
        fn test_invalid_retain_handling_validation() {
            let mut bits = SubscriptionOptionsBits::from_options(&SubscriptionOptions::default());
            bits.retain_handling = 3; // Invalid retain handling
            assert!(bits.to_options().is_err());
        }

        proptest! {
            #[test]
            fn prop_manual_vs_bebytes_encoding_consistency(
                qos in 0u8..=2,
                no_local: bool,
                retain_as_published: bool,
                retain_handling in 0u8..=2
            ) {
                let qos_enum = match qos {
                    0 => QoS::AtMostOnce,
                    1 => QoS::AtLeastOnce,
                    2 => QoS::ExactlyOnce,
                    _ => unreachable!(),
                };

                let retain_handling_enum = match retain_handling {
                    0 => RetainHandling::SendAtSubscribe,
                    1 => RetainHandling::SendAtSubscribeIfNew,
                    2 => RetainHandling::DoNotSend,
                    _ => unreachable!(),
                };

                let options = SubscriptionOptions {
                    qos: qos_enum,
                    no_local,
                    retain_as_published,
                    retain_handling: retain_handling_enum,
                };

                // Both encoding methods should produce identical results
                let manual_encoded = options.encode();
                let bebytes_encoded = options.encode_with_bebytes();
                prop_assert_eq!(manual_encoded, bebytes_encoded);

                // Both decoding methods should produce identical results
                let manual_decoded = SubscriptionOptions::decode(manual_encoded).unwrap();
                let bebytes_decoded = SubscriptionOptions::decode_with_bebytes(bebytes_encoded).unwrap();
                prop_assert_eq!(manual_decoded, bebytes_decoded);
                prop_assert_eq!(manual_decoded, options);
            }

            #[test]
            fn prop_bebytes_bit_field_round_trip(
                qos in 0u8..=2,
                no_local: bool,
                retain_as_published: bool,
                retain_handling in 0u8..=2
            ) {
                let bits = SubscriptionOptionsBits {
                    reserved_bits: 0,
                    retain_handling,
                    retain_as_published: u8::from(retain_as_published),
                    no_local: u8::from(no_local),
                    qos,
                };

                let bytes = bits.to_be_bytes();
                let (decoded, consumed) = SubscriptionOptionsBits::try_from_be_bytes(&bytes).unwrap();

                prop_assert_eq!(consumed, 1);
                prop_assert_eq!(decoded, bits);

                // Should be able to convert to high-level options
                let options = decoded.to_options().unwrap();
                prop_assert_eq!(options.qos as u8, qos);
                prop_assert_eq!(options.no_local, no_local);
                prop_assert_eq!(options.retain_as_published, retain_as_published);
                prop_assert_eq!(options.retain_handling as u8, retain_handling);
            }
        }
    }

    #[test]
    fn test_subscription_options_encode_decode() {
        let options = SubscriptionOptions {
            qos: QoS::AtLeastOnce,
            no_local: true,
            retain_as_published: true,
            retain_handling: RetainHandling::SendAtSubscribeIfNew,
        };

        let encoded = options.encode();
        assert_eq!(encoded, 0x1D); // QoS 1 + No Local + RAP + RH 1

        let decoded = SubscriptionOptions::decode(encoded).unwrap();
        assert_eq!(decoded, options);
    }

    #[test]
    fn test_subscribe_basic() {
        let packet = SubscribePacket::new(123)
            .add_filter("temperature/+", QoS::AtLeastOnce)
            .add_filter("humidity/#", QoS::ExactlyOnce);

        assert_eq!(packet.packet_id, 123);
        assert_eq!(packet.filters.len(), 2);
        assert_eq!(packet.filters[0].filter, "temperature/+");
        assert_eq!(packet.filters[0].options.qos, QoS::AtLeastOnce);
        assert_eq!(packet.filters[1].filter, "humidity/#");
        assert_eq!(packet.filters[1].options.qos, QoS::ExactlyOnce);
    }

    #[test]
    fn test_subscribe_with_options() {
        let options = SubscriptionOptions {
            qos: QoS::AtLeastOnce,
            no_local: true,
            retain_as_published: false,
            retain_handling: RetainHandling::DoNotSend,
        };

        let packet = SubscribePacket::new(456)
            .add_filter_with_options(TopicFilter::with_options("test/topic", options));

        assert!(packet.filters[0].options.no_local);
        assert_eq!(
            packet.filters[0].options.retain_handling,
            RetainHandling::DoNotSend
        );
    }

    #[test]
    fn test_subscribe_encode_decode() {
        let packet = SubscribePacket::new(789)
            .add_filter("sensor/temp", QoS::AtMostOnce)
            .add_filter("sensor/humidity", QoS::AtLeastOnce)
            .with_subscription_identifier(42);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::Subscribe);
        assert_eq!(fixed_header.flags, 0x02);

        let decoded = SubscribePacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.packet_id, 789);
        assert_eq!(decoded.filters.len(), 2);
        assert_eq!(decoded.filters[0].filter, "sensor/temp");
        assert_eq!(decoded.filters[0].options.qos, QoS::AtMostOnce);
        assert_eq!(decoded.filters[1].filter, "sensor/humidity");
        assert_eq!(decoded.filters[1].options.qos, QoS::AtLeastOnce);

        let sub_id = decoded.properties.get(PropertyId::SubscriptionIdentifier);
        assert!(sub_id.is_some());
    }

    #[test]
    fn test_subscribe_invalid_flags() {
        let mut buf = BytesMut::new();
        buf.put_u16(123);

        let fixed_header = FixedHeader::new(PacketType::Subscribe, 0x00, 2); // Wrong flags
        let result = SubscribePacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_subscribe_empty_filters() {
        let packet = SubscribePacket::new(123);

        let mut buf = BytesMut::new();
        let result = packet.encode(&mut buf);
        assert!(result.is_err());
    }
}
