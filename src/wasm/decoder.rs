use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, Packet};
use crate::transport::{Transport, WasmTransportType};
use bytes::Buf;

pub async fn read_packet(transport: &mut WasmTransportType) -> Result<Packet> {
    let mut header_buf = vec![0u8; 5];
    let n = transport.read(&mut header_buf).await?;

    if n == 0 {
        return Err(MqttError::ConnectionClosedByPeer);
    }

    let mut cursor = &header_buf[..n];
    let fixed_header = FixedHeader::decode(&mut cursor)?;

    let remaining_length = fixed_header.remaining_length as usize;
    let mut body_buf = vec![0u8; remaining_length];

    if remaining_length > 0 {
        let bytes_read = if cursor.remaining() > 0 {
            let available = cursor.remaining().min(remaining_length);
            body_buf[..available].copy_from_slice(&cursor[..available]);
            cursor.advance(available);
            available
        } else {
            0
        };

        if bytes_read < remaining_length {
            transport.read_exact(&mut body_buf[bytes_read..]).await?;
        }
    }

    let mut body = &body_buf[..];
    Packet::decode_from_body(fixed_header.packet_type, &fixed_header, &mut body)
}

#[allow(async_fn_in_trait)]
pub trait ReadExact {
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<()>;
}

impl ReadExact for WasmTransportType {
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut total_read = 0;
        while total_read < buf.len() {
            let n = self.read(&mut buf[total_read..]).await?;
            if n == 0 {
                return Err(MqttError::ConnectionClosedByPeer);
            }
            total_read += n;
        }
        Ok(())
    }
}
