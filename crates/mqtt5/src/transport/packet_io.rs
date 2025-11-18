//! Packet I/O utilities for transport layer
//!
//! Async methods for reading and writing MQTT packets.

use crate::encoding::encode_variable_int;
use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, MqttPacket, Packet, PacketType};
use crate::transport::tls::{TlsReadHalf, TlsWriteHalf};
use crate::transport::Transport;
use bytes::{BufMut, BytesMut};
use std::future::Future;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

/// Extension trait for Transport to add packet I/O methods
pub trait PacketIo: Transport {
    /// Read a complete MQTT packet
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn read_packet(&mut self) -> impl Future<Output = Result<Packet>> + Send + '_ {
        async move {
            // Read fixed header bytes
            let mut header_buf = BytesMut::with_capacity(5);

            // Read first byte (packet type and flags)
            let mut byte = [0u8; 1];
            tracing::trace!("Attempting to read first byte of packet");
            let n = self.read(&mut byte).await?;
            if n == 0 {
                tracing::debug!("Connection closed - received 0 bytes when reading packet header");
                return Err(MqttError::ClientClosed);
            }
            tracing::trace!(
                "Read first byte: 0x{:02x} (packet_type={}, flags={})",
                byte[0],
                (byte[0] >> 4) & 0x0f,
                byte[0] & 0x0f
            );
            header_buf.put_u8(byte[0]);

            // Read remaining length (variable length encoding)
            loop {
                let n = self.read(&mut byte).await?;
                if n == 0 {
                    tracing::debug!("Connection closed while reading remaining length");
                    return Err(MqttError::ClientClosed);
                }
                header_buf.put_u8(byte[0]);
                tracing::trace!("Read remaining length byte: 0x{:02x}", byte[0]);

                if (byte[0] & crate::constants::masks::CONTINUATION_BIT) == 0 {
                    break;
                }

                if header_buf.len() > 4 {
                    return Err(MqttError::MalformedPacket(
                        "Invalid remaining length encoding".to_string(),
                    ));
                }
            }

            // Parse the complete fixed header
            let mut header_buf = header_buf.freeze();
            tracing::trace!("Fixed header bytes: {:02x?}", header_buf.as_ref());
            let fixed_header = FixedHeader::decode(&mut header_buf)?;
            tracing::debug!(
                "Decoded fixed header: packet_type={:?}, flags=0x{:02x}, remaining_length={}",
                fixed_header.packet_type,
                fixed_header.flags,
                fixed_header.remaining_length
            );

            if fixed_header.remaining_length > 10000 {
                tracing::debug!(
                    packet_type = ?fixed_header.packet_type,
                    remaining_length = fixed_header.remaining_length,
                    "Receiving large packet"
                );
            }

            // Read remaining bytes
            let mut payload = vec![0u8; fixed_header.remaining_length as usize];
            let mut bytes_read = 0;
            while bytes_read < payload.len() {
                let n = self.read(&mut payload[bytes_read..]).await?;
                if n == 0 {
                    return Err(MqttError::ClientClosed);
                }
                bytes_read += n;
            }

            // Parse packet based on type
            let mut payload_buf = BytesMut::from(&payload[..]);

            if fixed_header.packet_type == PacketType::PubAck {
                tracing::trace!(payload_len = payload.len(), "Decoding PUBACK packet");
            }

            let packet = Packet::decode_from_body(
                fixed_header.packet_type,
                &fixed_header,
                &mut payload_buf,
            )?;
            tracing::debug!(
                "Successfully decoded packet: {:?}",
                fixed_header.packet_type
            );
            Ok(packet)
        }
    }

    /// Write a complete MQTT packet
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn write_packet(&mut self, packet: Packet) -> impl Future<Output = Result<()>> + Send + '_ {
        async move {
            let mut buf = BytesMut::with_capacity(1024);
            encode_packet_to_buffer(&packet, &mut buf)?;
            self.write(&buf).await?;
            Ok(())
        }
    }
}

/// Helper function to encode a packet with fixed header
fn encode_packet<F>(
    buf: &mut BytesMut,
    packet_type: PacketType,
    flags: u8,
    encode_body: F,
) -> Result<()>
where
    F: FnOnce(&mut BytesMut) -> Result<()>,
{
    // Encode body first to get remaining length
    let mut body_buf = BytesMut::new();
    encode_body(&mut body_buf)?;

    // Write fixed header
    let byte1 = (u8::from(packet_type) << 4) | (flags & crate::constants::masks::FLAGS);
    buf.put_u8(byte1);
    encode_variable_int(buf, u32::try_from(body_buf.len()).unwrap_or(u32::MAX))?;

    // Write body
    buf.put(body_buf);

    Ok(())
}

// Implement PacketIo for all types that implement Transport
impl<T: Transport> PacketIo for T {}

/// Packet reader trait for split read halves
pub trait PacketReader {
    /// Read a complete MQTT packet
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn read_packet(&mut self) -> impl Future<Output = Result<Packet>> + Send + '_;
}

/// Packet writer trait for split write halves  
pub trait PacketWriter {
    /// Write a complete MQTT packet
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn write_packet(&mut self, packet: Packet) -> impl Future<Output = Result<()>> + Send + '_;
}

/// Implementation for TCP read half
impl PacketReader for OwnedReadHalf {
    async fn read_packet(&mut self) -> Result<Packet> {
        // Read fixed header bytes
        let mut header_buf = BytesMut::with_capacity(5);

        // Read first byte (packet type and flags)
        let mut byte = [0u8; 1];
        let n = self.read(&mut byte).await?;
        if n == 0 {
            return Err(MqttError::ClientClosed);
        }
        header_buf.put_u8(byte[0]);

        // Read remaining length (variable length encoding)
        loop {
            let n = self.read(&mut byte).await?;
            if n == 0 {
                return Err(MqttError::ClientClosed);
            }
            header_buf.put_u8(byte[0]);

            if (byte[0] & crate::constants::masks::CONTINUATION_BIT) == 0 {
                break;
            }

            if header_buf.len() > 4 {
                return Err(MqttError::MalformedPacket(
                    "Invalid remaining length encoding".to_string(),
                ));
            }
        }

        // Parse the complete fixed header
        let mut header_buf = header_buf.freeze();
        let fixed_header = FixedHeader::decode(&mut header_buf)?;

        // Read remaining bytes
        let mut payload = vec![0u8; fixed_header.remaining_length as usize];
        let mut bytes_read = 0;
        while bytes_read < payload.len() {
            let n = self.read(&mut payload[bytes_read..]).await?;
            if n == 0 {
                return Err(MqttError::ClientClosed);
            }
            bytes_read += n;
        }

        // Parse packet based on type
        let mut payload_buf = BytesMut::from(&payload[..]);
        Packet::decode_from_body(fixed_header.packet_type, &fixed_header, &mut payload_buf)
    }
}

/// Helper function to encode any packet to a buffer
pub fn encode_packet_to_buffer(packet: &Packet, buf: &mut BytesMut) -> Result<()> {
    match packet {
        Packet::Connect(p) => {
            encode_packet(buf, PacketType::Connect, 0, |buf| p.encode_body(buf))?;
        }
        Packet::ConnAck(p) => {
            encode_packet(buf, PacketType::ConnAck, 0, |buf| p.encode_body(buf))?;
        }
        Packet::Publish(p) => {
            let flags = p.flags();
            encode_packet(buf, PacketType::Publish, flags, |buf| p.encode_body(buf))?;
        }
        Packet::PubAck(p) => {
            encode_packet(buf, PacketType::PubAck, 0, |buf| p.encode_body(buf))?;
        }
        Packet::PubRec(p) => {
            encode_packet(buf, PacketType::PubRec, 0, |buf| p.encode_body(buf))?;
        }
        Packet::PubRel(p) => {
            encode_packet(buf, PacketType::PubRel, 0x02, |buf| p.encode_body(buf))?;
        }
        Packet::PubComp(p) => {
            encode_packet(buf, PacketType::PubComp, 0, |buf| p.encode_body(buf))?;
        }
        Packet::Subscribe(p) => {
            encode_packet(buf, PacketType::Subscribe, 0x02, |buf| p.encode_body(buf))?;
        }
        Packet::SubAck(p) => {
            encode_packet(buf, PacketType::SubAck, 0, |buf| p.encode_body(buf))?;
        }
        Packet::Unsubscribe(p) => {
            encode_packet(buf, PacketType::Unsubscribe, 0x02, |buf| p.encode_body(buf))?;
        }
        Packet::UnsubAck(p) => {
            encode_packet(buf, PacketType::UnsubAck, 0, |buf| p.encode_body(buf))?;
        }
        Packet::PingReq => encode_packet(buf, PacketType::PingReq, 0, |_| Ok(()))?,
        Packet::PingResp => encode_packet(buf, PacketType::PingResp, 0, |_| Ok(()))?,
        Packet::Disconnect(p) => {
            encode_packet(buf, PacketType::Disconnect, 0, |buf| p.encode_body(buf))?;
        }
        Packet::Auth(p) => {
            encode_packet(buf, PacketType::Auth, 0, |buf| p.encode_body(buf))?;
        }
    }
    Ok(())
}

/// Implementation for TCP write half
impl PacketWriter for OwnedWriteHalf {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        let mut buf = BytesMut::with_capacity(1024);
        encode_packet_to_buffer(&packet, &mut buf)?;
        self.write_all(&buf).await?;
        self.flush().await?;
        Ok(())
    }
}

/// Implementation for TLS read half
impl PacketReader for TlsReadHalf {
    async fn read_packet(&mut self) -> Result<Packet> {
        // Read fixed header bytes
        let mut header_buf = BytesMut::with_capacity(5);

        // Read first byte (packet type and flags)
        let mut byte = [0u8; 1];
        let n = self.read(&mut byte).await?;
        if n == 0 {
            return Err(MqttError::ClientClosed);
        }
        header_buf.put_u8(byte[0]);

        // Read remaining length (variable length encoding)
        loop {
            let n = self.read(&mut byte).await?;
            if n == 0 {
                return Err(MqttError::ClientClosed);
            }
            header_buf.put_u8(byte[0]);

            if (byte[0] & crate::constants::masks::CONTINUATION_BIT) == 0 {
                break;
            }

            if header_buf.len() > 4 {
                return Err(MqttError::MalformedPacket(
                    "Invalid remaining length encoding".to_string(),
                ));
            }
        }

        // Parse the complete fixed header
        let mut header_buf = header_buf.freeze();
        let fixed_header = FixedHeader::decode(&mut header_buf)?;

        // Read remaining bytes
        let mut payload = vec![0u8; fixed_header.remaining_length as usize];
        let mut bytes_read = 0;
        while bytes_read < payload.len() {
            let n = self.read(&mut payload[bytes_read..]).await?;
            if n == 0 {
                return Err(MqttError::ClientClosed);
            }
            bytes_read += n;
        }

        // Parse packet based on type
        let mut payload_buf = BytesMut::from(&payload[..]);
        Packet::decode_from_body(fixed_header.packet_type, &fixed_header, &mut payload_buf)
    }
}

/// Implementation for TLS write half
impl PacketWriter for TlsWriteHalf {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        let mut buf = BytesMut::with_capacity(1024);
        encode_packet_to_buffer(&packet, &mut buf)?;
        self.write_all(&buf).await?;
        self.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::connack::ConnAckPacket;
    use crate::packet::publish::PublishPacket;
    use crate::packet::subscribe::{SubscribePacket, SubscriptionOptions, TopicFilter};
    use crate::protocol::v5::properties::Properties;
    use crate::protocol::v5::reason_codes::ReasonCode;
    use crate::transport::mock::MockTransport;
    use crate::QoS;

    #[tokio::test]
    async fn test_read_packet_pingresp() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        // Inject a PINGRESP packet
        transport
            .add_incoming_data(&crate::constants::packets::PINGRESP_BYTES)
            .await;

        let packet = transport.read_packet().await.unwrap();
        assert!(matches!(packet, Packet::PingResp));
    }

    #[tokio::test]
    async fn test_read_packet_pingreq() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        // Inject a PINGREQ packet
        transport
            .add_incoming_data(&crate::constants::packets::PINGREQ_BYTES)
            .await;

        let packet = transport.read_packet().await.unwrap();
        assert!(matches!(packet, Packet::PingReq));
    }

    #[tokio::test]
    async fn test_read_packet_connack() {
        use crate::packet::connack::ConnAckPacket;

        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        // Create a CONNACK packet using proper encoding
        let connack = ConnAckPacket {
            protocol_version: 5,
            session_present: false,
            reason_code: ReasonCode::Success,
            properties: Properties::new(),
        };

        let mut data = BytesMut::new();
        connack.encode(&mut data).unwrap();
        transport.add_incoming_data(&data).await;

        let packet = transport.read_packet().await.unwrap();
        match packet {
            Packet::ConnAck(connack) => {
                assert!(!connack.session_present);
                assert_eq!(connack.reason_code, ReasonCode::Success);
            }
            _ => panic!("Expected CONNACK packet"),
        }
    }

    #[tokio::test]
    async fn test_read_packet_publish() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        // Create a PUBLISH packet with QoS 0
        let topic = "test/topic";
        let payload = b"Hello MQTT";

        // Use proper encoding
        let mut buf = BytesMut::new();

        // Encode topic string using the proper function
        crate::encoding::encode_string(&mut buf, topic).unwrap();

        // Properties length (0 for no properties)
        buf.put_u8(0x00);

        // Payload
        buf.extend_from_slice(payload);

        // Now create the full packet with fixed header
        let mut data = BytesMut::new();
        data.put_u8(0x30); // PUBLISH with QoS 0
        crate::encoding::encode_variable_int(&mut data, u32::try_from(buf.len()).unwrap()).unwrap();
        data.extend_from_slice(&buf);

        transport.add_incoming_data(&data).await;

        let packet = transport.read_packet().await.unwrap();
        match packet {
            Packet::Publish(publish) => {
                assert_eq!(publish.topic_name, "test/topic");
                assert_eq!(publish.payload, b"Hello MQTT");
                assert_eq!(publish.qos, QoS::AtMostOnce);
                assert_eq!(publish.packet_id, None);
            }
            _ => panic!("Expected PUBLISH packet"),
        }
    }

    #[tokio::test]
    async fn test_read_packet_invalid_remaining_length() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        // Create packet with invalid remaining length (5 bytes with continuation bit)
        // This must be manually constructed as it's testing invalid encoding
        let mut data = BytesMut::new();
        data.put_u8(crate::constants::fixed_header::PUBLISH_BASE);
        // Invalid variable byte integer - 5 bytes all with continuation bit
        data.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        transport.add_incoming_data(&data).await;

        let result = transport.read_packet().await;
        assert!(result.is_err());
        if let Err(e) = result {
            match e {
                MqttError::MalformedPacket(_) => {}
                _ => panic!("Expected MalformedPacket error, got: {e:?}"),
            }
        }
    }

    #[tokio::test]
    async fn test_read_packet_connection_closed() {
        let mut transport = MockTransport::new();

        // Don't add any data - read should return 0
        let result = transport.read_packet().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_write_packet_pingreq() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        transport.write_packet(Packet::PingReq).await.unwrap();

        let written = transport.get_written_data().await;
        assert_eq!(written, crate::constants::packets::PINGREQ_BYTES.to_vec()); // PINGREQ packet
    }

    #[tokio::test]
    async fn test_write_packet_publish() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        let publish = PublishPacket {
            topic_name: "test".to_string(),
            payload: vec![1, 2, 3],
            qos: QoS::AtLeastOnce,
            retain: false,
            dup: false,
            packet_id: Some(123),
            properties: Properties::new(),
        };

        transport
            .write_packet(Packet::Publish(publish))
            .await
            .unwrap();

        let written = transport.get_written_data().await;

        // Verify fixed header
        assert_eq!(written[0] >> 4, u8::from(PacketType::Publish));
        assert_eq!(written[0] & crate::constants::masks::FLAGS, 0x02); // QoS 1 flag

        // Should contain topic, packet ID, and payload
        assert!(written.len() > 2 + 4 + 2 + 3); // header + topic + packet_id + payload
    }

    #[tokio::test]
    async fn test_write_packet_subscribe() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        let subscribe = SubscribePacket {
            packet_id: 456,
            properties: Properties::new(),
            filters: vec![TopicFilter {
                filter: "test/+".to_string(),
                options: SubscriptionOptions {
                    qos: QoS::AtLeastOnce,
                    no_local: false,
                    retain_as_published: false,
                    retain_handling: crate::packet::subscribe::RetainHandling::SendAtSubscribe,
                },
            }],
        };

        transport
            .write_packet(Packet::Subscribe(subscribe))
            .await
            .unwrap();

        let written = transport.get_written_data().await;

        // Verify fixed header
        assert_eq!(written[0], 0x82); // SUBSCRIBE with required flags
        assert!(written.len() > 2); // Has content
    }

    #[tokio::test]
    async fn test_roundtrip_packets() {
        // Test that we can write and read back various packet types
        let test_packets = vec![
            Packet::PingReq,
            Packet::PingResp,
            Packet::ConnAck(ConnAckPacket {
                session_present: true,
                reason_code: ReasonCode::Success,
                properties: Properties::new(),
                protocol_version: 5,
            }),
        ];

        for packet in test_packets {
            let mut write_transport = MockTransport::new();
            write_transport.connect().await.unwrap();
            write_transport.write_packet(packet.clone()).await.unwrap();

            let data = write_transport.get_written_data().await;

            let mut read_transport = MockTransport::new();
            read_transport.connect().await.unwrap();
            read_transport.add_incoming_data(&data).await;

            let read_packet = read_transport.read_packet().await.unwrap();

            // Basic type check
            match (&packet, &read_packet) {
                (Packet::PingReq, Packet::PingReq) | (Packet::PingResp, Packet::PingResp) => {}
                (Packet::ConnAck(a), Packet::ConnAck(b)) => {
                    assert_eq!(a.session_present, b.session_present);
                    assert_eq!(a.reason_code, b.reason_code);
                }
                _ => panic!("Packet type mismatch"),
            }
        }
    }

    #[tokio::test]
    async fn test_encode_packet_helper() {
        let mut buf = BytesMut::new();

        // Test encoding a simple packet
        encode_packet(&mut buf, PacketType::PingReq, 0, |_| Ok(())).unwrap();

        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0], crate::constants::fixed_header::PINGREQ); // PINGREQ type
        assert_eq!(buf[1], 0x00); // Zero length
    }

    #[tokio::test]
    async fn test_variable_length_encoding() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        // Create a publish with large payload to test variable length encoding
        let mut large_payload = vec![0u8; 200];
        for (i, byte) in large_payload.iter_mut().enumerate() {
            *byte = u8::try_from(i % 256).expect("modulo 256 always fits in u8");
        }

        let publish = PublishPacket {
            topic_name: "test".to_string(),
            payload: large_payload,
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            packet_id: None,
            properties: Properties::new(),
        };

        transport
            .write_packet(Packet::Publish(publish))
            .await
            .unwrap();

        let written = transport.get_written_data().await;

        // Verify the remaining length uses 2 bytes (since payload > 127)
        assert!(written[1] & crate::constants::masks::CONTINUATION_BIT != 0); // Continuation bit set
        assert!(written.len() > 200); // Contains the large payload
    }
}
