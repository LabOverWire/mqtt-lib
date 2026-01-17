use crate::error::{MqttError, Result};
use crate::prelude::{format, ToString};
use crate::QoS;
use bebytes::BeBytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RetainHandling {
    SendAtSubscribe = 0,
    SendAtSubscribeIfNew = 1,
    DoNotSend = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BeBytes)]
pub struct SubscriptionOptionsBits {
    #[bits(2)]
    pub reserved_bits: u8,
    #[bits(2)]
    pub retain_handling: u8,
    #[bits(1)]
    pub retain_as_published: u8,
    #[bits(1)]
    pub no_local: u8,
    #[bits(2)]
    pub qos: u8,
}

impl SubscriptionOptionsBits {
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

    /// # Errors
    /// Returns an error if reserved bits are set, or if `QoS` or retain handling values are invalid.
    pub fn to_options(&self) -> Result<SubscriptionOptions> {
        if self.reserved_bits != 0 {
            return Err(MqttError::MalformedPacket(
                "Reserved bits in subscription options must be 0".to_string(),
            ));
        }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionOptions {
    pub qos: QoS,
    pub no_local: bool,
    pub retain_as_published: bool,
    pub retain_handling: RetainHandling,
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
    #[must_use]
    pub fn new(qos: QoS) -> Self {
        Self {
            qos,
            ..Default::default()
        }
    }

    #[must_use]
    pub fn with_qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

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

    #[must_use]
    pub fn encode_with_bebytes(&self) -> u8 {
        let bits = SubscriptionOptionsBits::from_options(self);
        bits.to_be_bytes()[0]
    }

    /// # Errors
    /// Returns an error if the `QoS` value or retain handling is invalid, or reserved bits are set.
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

    /// # Errors
    /// Returns an error if the `QoS` value or retain handling is invalid, or reserved bits are set.
    pub fn decode_with_bebytes(byte: u8) -> Result<Self> {
        let (bits, _consumed) =
            SubscriptionOptionsBits::try_from_be_bytes(&[byte]).map_err(|e| {
                MqttError::MalformedPacket(format!("Invalid subscription options byte: {e}"))
            })?;

        bits.to_options()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn test_bebytes_vs_manual_encoding_identical() {
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

            let manual_decoded = SubscriptionOptions::decode(manual_encoded).unwrap();
            let bebytes_decoded =
                SubscriptionOptions::decode_with_bebytes(bebytes_encoded).unwrap();

            assert_eq!(manual_decoded, bebytes_decoded);
            assert_eq!(manual_decoded, options);
        }
    }

    #[test]
    fn test_subscription_options_bits_round_trip() {
        use bebytes::BeBytes;
        let options = SubscriptionOptions {
            qos: QoS::AtLeastOnce,
            no_local: true,
            retain_as_published: false,
            retain_handling: RetainHandling::SendAtSubscribeIfNew,
        };

        let bits = SubscriptionOptionsBits::from_options(&options);
        let bytes = bits.to_be_bytes();
        assert_eq!(bytes.len(), 1);

        let (decoded_bits, consumed) = SubscriptionOptionsBits::try_from_be_bytes(&bytes).unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(decoded_bits, bits);

        let decoded_options = decoded_bits.to_options().unwrap();
        assert_eq!(decoded_options, options);
    }

    #[test]
    fn test_reserved_bits_validation() {
        let mut bits = SubscriptionOptionsBits::from_options(&SubscriptionOptions::default());

        bits.reserved_bits = 1;
        assert!(bits.to_options().is_err());

        bits.reserved_bits = 2;
        assert!(bits.to_options().is_err());
    }

    #[test]
    fn test_invalid_qos_validation() {
        let mut bits = SubscriptionOptionsBits::from_options(&SubscriptionOptions::default());
        bits.qos = 3;
        assert!(bits.to_options().is_err());
    }

    #[test]
    fn test_invalid_retain_handling_validation() {
        let mut bits = SubscriptionOptionsBits::from_options(&SubscriptionOptions::default());
        bits.retain_handling = 3;
        assert!(bits.to_options().is_err());
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
        assert_eq!(encoded, 0x1D);

        let decoded = SubscriptionOptions::decode(encoded).unwrap();
        assert_eq!(decoded, options);
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

            let manual_encoded = options.encode();
            let bebytes_encoded = options.encode_with_bebytes();
            prop_assert_eq!(manual_encoded, bebytes_encoded);

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
            use bebytes::BeBytes;
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

            let options = decoded.to_options().unwrap();
            prop_assert_eq!(options.qos as u8, qos);
            prop_assert_eq!(options.no_local, no_local);
            prop_assert_eq!(options.retain_as_published, retain_as_published);
            prop_assert_eq!(options.retain_handling as u8, retain_handling);
        }
    }
}
