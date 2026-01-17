use super::{Properties, PropertyId, PropertyValue, PropertyValueType};
use crate::encoding::{
    decode_binary, decode_string, decode_variable_int, encode_binary, encode_string,
    encode_variable_int,
};
use crate::error::{MqttError, Result};
use crate::prelude::{format, ToString};
use bytes::{Buf, BufMut};

impl Properties {
    /// # Errors
    /// Returns error if encoding fails or packet is too large.
    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        let mut props_buf = crate::prelude::Vec::new();
        self.encode_properties(&mut props_buf)?;

        encode_variable_int(
            buf,
            props_buf
                .len()
                .try_into()
                .map_err(|_| MqttError::PacketTooLarge {
                    size: props_buf.len(),
                    max: u32::MAX as usize,
                })?,
        )?;

        buf.put_slice(&props_buf);
        Ok(())
    }

    fn encode_properties<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        for (id, values) in &self.properties {
            for value in values {
                encode_variable_int(buf, u32::from(*id as u8))?;

                match value {
                    PropertyValue::Byte(v) => buf.put_u8(*v),
                    PropertyValue::TwoByteInteger(v) => buf.put_u16(*v),
                    PropertyValue::FourByteInteger(v) => buf.put_u32(*v),
                    PropertyValue::VariableByteInteger(v) => encode_variable_int(buf, *v)?,
                    PropertyValue::BinaryData(v) => encode_binary(buf, v)?,
                    PropertyValue::Utf8String(v) => encode_string(buf, v)?,
                    PropertyValue::Utf8StringPair(k, v) => {
                        encode_string(buf, k)?;
                        encode_string(buf, v)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// # Errors
    /// Returns error if encoding fails or packet is too large.
    pub fn encode_direct<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        let props_len = self.properties_encoded_len();
        encode_variable_int(
            buf,
            props_len
                .try_into()
                .map_err(|_| MqttError::PacketTooLarge {
                    size: props_len,
                    max: u32::MAX as usize,
                })?,
        )?;
        self.encode_properties(buf)
    }

    /// # Errors
    /// Returns error if decoding fails, invalid property ID, type mismatch,
    /// or duplicate property that doesn't allow multiples.
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self> {
        let props_len = decode_variable_int(buf)? as usize;

        if buf.remaining() < props_len {
            return Err(MqttError::MalformedPacket(format!(
                "Insufficient data for properties: expected {props_len}, got {}",
                buf.remaining()
            )));
        }

        let mut props_buf = buf.copy_to_bytes(props_len);
        let mut properties = Self::new();

        while props_buf.has_remaining() {
            let id_val = decode_variable_int(&mut props_buf)?;
            let id_byte = u8::try_from(id_val).map_err(|_| MqttError::InvalidPropertyId(255))?;

            let id = PropertyId::from_u8(id_byte).ok_or(MqttError::InvalidPropertyId(id_byte))?;

            let value = match id.value_type() {
                PropertyValueType::Byte => {
                    if !props_buf.has_remaining() {
                        return Err(MqttError::MalformedPacket(
                            "Insufficient data for byte property".to_string(),
                        ));
                    }
                    PropertyValue::Byte(props_buf.get_u8())
                }
                PropertyValueType::TwoByteInteger => {
                    if props_buf.remaining() < 2 {
                        return Err(MqttError::MalformedPacket(
                            "Insufficient data for two-byte integer property".to_string(),
                        ));
                    }
                    PropertyValue::TwoByteInteger(props_buf.get_u16())
                }
                PropertyValueType::FourByteInteger => {
                    if props_buf.remaining() < 4 {
                        return Err(MqttError::MalformedPacket(
                            "Insufficient data for four-byte integer property".to_string(),
                        ));
                    }
                    PropertyValue::FourByteInteger(props_buf.get_u32())
                }
                PropertyValueType::VariableByteInteger => {
                    PropertyValue::VariableByteInteger(decode_variable_int(&mut props_buf)?)
                }
                PropertyValueType::BinaryData => {
                    PropertyValue::BinaryData(decode_binary(&mut props_buf)?)
                }
                PropertyValueType::Utf8String => {
                    PropertyValue::Utf8String(decode_string(&mut props_buf)?)
                }
                PropertyValueType::Utf8StringPair => {
                    let key = decode_string(&mut props_buf)?;
                    let value = decode_string(&mut props_buf)?;
                    PropertyValue::Utf8StringPair(key, value)
                }
            };

            properties.add(id, value)?;
        }

        Ok(properties)
    }

    #[must_use]
    pub fn encoded_len(&self) -> usize {
        let props_len = self.properties_encoded_len();
        crate::encoding::variable_int_len(props_len.try_into().unwrap_or(u32::MAX)) + props_len
    }

    #[must_use]
    fn properties_encoded_len(&self) -> usize {
        let mut len = 0;

        for (id, values) in &self.properties {
            for value in values {
                len += crate::encoding::variable_int_len(u32::from(*id as u8));

                len += match value {
                    PropertyValue::Byte(_) => 1,
                    PropertyValue::TwoByteInteger(_) => 2,
                    PropertyValue::FourByteInteger(_) => 4,
                    PropertyValue::VariableByteInteger(v) => crate::encoding::variable_int_len(*v),
                    PropertyValue::BinaryData(v) => crate::encoding::binary_len(v),
                    PropertyValue::Utf8String(v) => crate::encoding::string_len(v),
                    PropertyValue::Utf8StringPair(k, v) => {
                        crate::encoding::string_len(k) + crate::encoding::string_len(v)
                    }
                };
            }
        }

        len
    }
}
