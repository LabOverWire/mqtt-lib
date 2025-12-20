use crate::types::ReasonCode;

macro_rules! define_ack_packet {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident;
        packet_type = $packet_type:expr;
        validator = $validator:path;
        error_prefix = $error_prefix:literal;
    ) => {
        $crate::packet::ack_common::define_ack_packet_inner! {
            $(#[$meta])*
            $vis struct $name;
            packet_type = $packet_type;
            validator = $validator;
            error_prefix = $error_prefix;
            flags = (None);
        }
    };
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident;
        packet_type = $packet_type:expr;
        validator = $validator:path;
        error_prefix = $error_prefix:literal;
        flags = $flags:literal;
        validate_flags = true;
    ) => {
        $crate::packet::ack_common::define_ack_packet_inner! {
            $(#[$meta])*
            $vis struct $name;
            packet_type = $packet_type;
            validator = $validator;
            error_prefix = $error_prefix;
            flags = (Some($flags));
        }
    };
}

#[doc(hidden)]
macro_rules! ack_impl_flags {
    ((None)) => {};
    ((Some($val:literal))) => {
        fn flags(&self) -> u8 {
            $val
        }
    };
}

#[doc(hidden)]
macro_rules! ack_validate_flags {
    ((None), $fh:ident, $prefix:literal) => {
        let _ = $fh;
    };
    ((Some($val:literal)), $fh:ident, $prefix:literal) => {
        if $fh.flags != $val {
            return Err($crate::error::MqttError::MalformedPacket(format!(
                concat!(
                    "Invalid ",
                    $prefix,
                    " flags: expected 0x{:02X}, got 0x{:02X}"
                ),
                $val, $fh.flags
            )));
        }
    };
}

#[doc(hidden)]
macro_rules! define_ack_packet_inner {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident;
        packet_type = $packet_type:expr;
        validator = $validator:path;
        error_prefix = $error_prefix:literal;
        flags = $flags:tt;
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        $vis struct $name {
            pub packet_id: u16,
            pub reason_code: $crate::types::ReasonCode,
            pub properties: $crate::protocol::v5::properties::Properties,
        }

        impl $name {
            #[must_use]
            pub fn new(packet_id: u16) -> Self {
                Self {
                    packet_id,
                    reason_code: $crate::types::ReasonCode::Success,
                    properties: $crate::protocol::v5::properties::Properties::default(),
                }
            }

            #[must_use]
            pub fn new_with_reason(packet_id: u16, reason_code: $crate::types::ReasonCode) -> Self {
                Self {
                    packet_id,
                    reason_code,
                    properties: $crate::protocol::v5::properties::Properties::default(),
                }
            }

            #[must_use]
            pub fn with_reason_string(mut self, reason: String) -> Self {
                self.properties.set_reason_string(reason);
                self
            }

            #[must_use]
            pub fn with_user_property(mut self, key: String, value: String) -> Self {
                self.properties.add_user_property(key, value);
                self
            }
        }

        impl $crate::packet::MqttPacket for $name {
            fn packet_type(&self) -> $crate::packet::PacketType {
                $packet_type
            }

            $crate::packet::ack_common::ack_impl_flags!($flags);

            fn encode_body<B: bytes::BufMut>(&self, buf: &mut B) -> $crate::error::Result<()> {
                buf.put_u16(self.packet_id);
                if self.reason_code != $crate::types::ReasonCode::Success || !self.properties.is_empty() {
                    buf.put_u8(u8::from(self.reason_code));
                    self.properties.encode(buf)?;
                }
                Ok(())
            }

            fn decode_body<B: bytes::Buf>(
                buf: &mut B,
                fixed_header: &$crate::packet::FixedHeader,
            ) -> $crate::error::Result<Self> {
                $crate::packet::ack_common::ack_validate_flags!($flags, fixed_header, $error_prefix);

                if buf.remaining() < 2 {
                    return Err($crate::error::MqttError::MalformedPacket(
                        concat!($error_prefix, " missing packet identifier").to_string(),
                    ));
                }
                let packet_id = buf.get_u16();

                let (reason_code, properties) = if buf.has_remaining() {
                    let reason_byte = buf.get_u8();
                    let code = $crate::types::ReasonCode::from_u8(reason_byte).ok_or_else(|| {
                        $crate::error::MqttError::MalformedPacket(format!(
                            concat!("Invalid ", $error_prefix, " reason code: {}"),
                            reason_byte
                        ))
                    })?;

                    if !$validator(code) {
                        return Err($crate::error::MqttError::MalformedPacket(format!(
                            concat!("Invalid ", $error_prefix, " reason code: {:?}"),
                            code
                        )));
                    }

                    let props = if buf.has_remaining() {
                        $crate::protocol::v5::properties::Properties::decode(buf)?
                    } else {
                        $crate::protocol::v5::properties::Properties::default()
                    };

                    (code, props)
                } else {
                    ($crate::types::ReasonCode::Success, $crate::protocol::v5::properties::Properties::default())
                };

                Ok(Self {
                    packet_id,
                    reason_code,
                    properties,
                })
            }
        }
    };
}

pub(crate) use ack_impl_flags;
pub(crate) use ack_validate_flags;
pub(crate) use define_ack_packet;
pub(crate) use define_ack_packet_inner;

#[must_use]
pub fn is_valid_publish_ack_reason_code(code: ReasonCode) -> bool {
    matches!(
        code,
        ReasonCode::Success
            | ReasonCode::NoMatchingSubscribers
            | ReasonCode::UnspecifiedError
            | ReasonCode::ImplementationSpecificError
            | ReasonCode::NotAuthorized
            | ReasonCode::TopicNameInvalid
            | ReasonCode::PacketIdentifierInUse
            | ReasonCode::QuotaExceeded
            | ReasonCode::PayloadFormatInvalid
    )
}

#[must_use]
pub fn is_valid_pubrel_reason_code(code: ReasonCode) -> bool {
    matches!(
        code,
        ReasonCode::Success | ReasonCode::PacketIdentifierNotFound
    )
}
