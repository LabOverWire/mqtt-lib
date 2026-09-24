use crate::transport::WasmReader;
use mqtt5_protocol::constants::limits::MAX_PACKET_SIZE;
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::packet::{FixedHeader, Packet, PacketType};

const MAX_REMAINING_LENGTH_BYTES: usize = 4;

/// # Errors
/// Returns an error if the connection is closed or packet decoding fails.
pub async fn read_packet(reader: &mut WasmReader) -> Result<Packet> {
    let (fixed_header, body) = read_frame(reader, MAX_PACKET_SIZE).await?;
    Packet::decode_from_body(
        fixed_header.packet_type,
        &fixed_header,
        &mut body.as_slice(),
    )
}

pub(crate) async fn read_frame(
    reader: &mut WasmReader,
    max_packet_size: u32,
) -> Result<(FixedHeader, Vec<u8>)> {
    let first_byte = read_byte(reader).await?;
    let packet_type_value = first_byte >> 4;
    let packet_type = PacketType::from_u8(packet_type_value)
        .ok_or(MqttError::InvalidPacketType(packet_type_value))?;

    let mut remaining_length: u32 = 0;
    let mut length_bytes = 0;
    loop {
        if length_bytes == MAX_REMAINING_LENGTH_BYTES {
            return Err(MqttError::MalformedPacket(
                "Remaining Length exceeds four bytes".to_string(),
            ));
        }
        let byte = read_byte(reader).await?;
        remaining_length |= u32::from(byte & 0x7F) << (7 * length_bytes);
        length_bytes += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }

    let header_len = 1 + length_bytes;
    let total = usize::try_from(remaining_length)
        .ok()
        .and_then(|len| len.checked_add(header_len))
        .ok_or_else(|| MqttError::MalformedPacket("Remaining Length overflow".to_string()))?;
    let max = usize::try_from(max_packet_size).unwrap_or(usize::MAX);
    if total > max {
        return Err(MqttError::PacketTooLarge { size: total, max });
    }

    let mut body = vec![0u8; total - header_len];
    read_exact(reader, &mut body).await?;

    let fixed_header = FixedHeader::new(packet_type, first_byte & 0x0F, remaining_length);
    Ok((fixed_header, body))
}

async fn read_byte(reader: &mut WasmReader) -> Result<u8> {
    let mut byte = [0u8; 1];
    read_exact(reader, &mut byte).await?;
    Ok(byte[0])
}

async fn read_exact(reader: &mut WasmReader, buf: &mut [u8]) -> Result<()> {
    let mut total_read = 0;
    while total_read < buf.len() {
        let n = reader.read(&mut buf[total_read..]).await?;
        if n == 0 {
            return Err(MqttError::ConnectionClosedByPeer);
        }
        total_read += n;
    }
    Ok(())
}
