use crate::error::{MqttError, Result};
use bebytes::BeBytes;
use bytes::{Buf, BufMut, Bytes, BytesMut};

pub const FLOW_TYPE_CONTROL: u8 = 0x11;
pub const FLOW_TYPE_CLIENT_DATA: u8 = 0x12;
pub const FLOW_TYPE_SERVER_DATA: u8 = 0x13;
pub const FLOW_TYPE_USER_DEFINED: u8 = 0x14;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlowId(u64);

impl FlowId {
    pub fn client(id: u64) -> Self {
        Self((id << 1) & !1)
    }

    pub fn server(id: u64) -> Self {
        Self((id << 1) | 1)
    }

    pub fn control() -> Self {
        Self(0)
    }

    pub fn is_client_initiated(&self) -> bool {
        self.0 & 1 == 0
    }

    pub fn is_server_initiated(&self) -> bool {
        self.0 & 1 == 1
    }

    pub fn raw(&self) -> u64 {
        self.0
    }

    pub fn sequence(&self) -> u64 {
        self.0 >> 1
    }
}

impl From<u64> for FlowId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, BeBytes)]
pub struct FlowFlags {
    #[bits(1)]
    pub optional_headers: u8,
    #[bits(1)]
    pub persistent_subscriptions: u8,
    #[bits(1)]
    pub persistent_topic_alias: u8,
    #[bits(1)]
    pub persistent_qos: u8,
    #[bits(2)]
    pub err_tolerance: u8,
    #[bits(1)]
    pub abort_if_no_state: u8,
    #[bits(1)]
    pub clean: u8,
}

impl FlowFlags {
    pub fn encode(&self) -> u8 {
        self.to_be_bytes()[0]
    }

    pub fn decode(byte: u8) -> Self {
        match Self::try_from_be_bytes(&[byte]) {
            Ok((flags, _)) => flags,
            Err(_) => Self::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ControlFlowHeader {
    pub flow_id: FlowId,
    pub flags: FlowFlags,
}

impl ControlFlowHeader {
    pub fn new(flags: FlowFlags) -> Self {
        Self {
            flow_id: FlowId::control(),
            flags,
        }
    }

    pub fn encode(&self, buf: &mut BytesMut) {
        encode_varint(FLOW_TYPE_CONTROL as u64, buf);
        encode_varint(self.flow_id.raw(), buf);
        buf.put_u8(self.flags.encode());
    }

    pub fn decode(buf: &mut Bytes) -> Result<Self> {
        let flow_id = FlowId::from(decode_varint(buf)?);
        if buf.remaining() < 1 {
            return Err(MqttError::ProtocolError("missing flow flags".into()));
        }
        let flags = FlowFlags::decode(buf.get_u8());
        Ok(Self { flow_id, flags })
    }
}

#[derive(Debug, Clone)]
pub struct DataFlowHeader {
    pub flow_type: u8,
    pub flow_id: FlowId,
    pub expire_interval: u64,
    pub flags: FlowFlags,
}

impl DataFlowHeader {
    pub fn client(flow_id: FlowId, expire_interval: u64, flags: FlowFlags) -> Self {
        Self {
            flow_type: FLOW_TYPE_CLIENT_DATA,
            flow_id,
            expire_interval,
            flags,
        }
    }

    pub fn server(flow_id: FlowId, expire_interval: u64, flags: FlowFlags) -> Self {
        Self {
            flow_type: FLOW_TYPE_SERVER_DATA,
            flow_id,
            expire_interval,
            flags,
        }
    }

    pub fn encode(&self, buf: &mut BytesMut) {
        encode_varint(self.flow_type as u64, buf);
        encode_varint(self.flow_id.raw(), buf);
        encode_varint(self.expire_interval, buf);
        buf.put_u8(self.flags.encode());
    }

    pub fn decode(flow_type: u8, buf: &mut Bytes) -> Result<Self> {
        let flow_id = FlowId::from(decode_varint(buf)?);
        let expire_interval = decode_varint(buf)?;
        if buf.remaining() < 1 {
            return Err(MqttError::ProtocolError("missing flow flags".into()));
        }
        let flags = FlowFlags::decode(buf.get_u8());
        Ok(Self {
            flow_type,
            flow_id,
            expire_interval,
            flags,
        })
    }

    pub fn is_client_flow(&self) -> bool {
        self.flow_type == FLOW_TYPE_CLIENT_DATA
    }

    pub fn is_server_flow(&self) -> bool {
        self.flow_type == FLOW_TYPE_SERVER_DATA
    }
}

#[derive(Debug, Clone)]
pub enum FlowHeader {
    Control(ControlFlowHeader),
    ClientData(DataFlowHeader),
    ServerData(DataFlowHeader),
    UserDefined(Bytes),
}

impl FlowHeader {
    pub fn encode(&self, buf: &mut BytesMut) {
        match self {
            Self::Control(h) => h.encode(buf),
            Self::ClientData(h) | Self::ServerData(h) => h.encode(buf),
            Self::UserDefined(data) => {
                encode_varint(FLOW_TYPE_USER_DEFINED as u64, buf);
                buf.extend_from_slice(data);
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn decode(buf: &mut Bytes) -> Result<Self> {
        let flow_type = decode_varint(buf)? as u8;
        match flow_type {
            FLOW_TYPE_CONTROL => Ok(Self::Control(ControlFlowHeader::decode(buf)?)),
            FLOW_TYPE_CLIENT_DATA => Ok(Self::ClientData(DataFlowHeader::decode(flow_type, buf)?)),
            FLOW_TYPE_SERVER_DATA => Ok(Self::ServerData(DataFlowHeader::decode(flow_type, buf)?)),
            FLOW_TYPE_USER_DEFINED => Ok(Self::UserDefined(buf.split_to(buf.remaining()))),
            _ => Err(MqttError::ProtocolError(format!(
                "unknown flow type: {flow_type}"
            ))),
        }
    }

    pub fn flow_id(&self) -> Option<FlowId> {
        match self {
            Self::Control(h) => Some(h.flow_id),
            Self::ClientData(h) | Self::ServerData(h) => Some(h.flow_id),
            Self::UserDefined(_) => None,
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
pub fn encode_varint(value: u64, buf: &mut BytesMut) {
    if value <= 63 {
        buf.put_u8(value as u8);
    } else if value <= 16383 {
        buf.put_u8(((value >> 8) as u8) | 0b0100_0000);
        buf.put_u8(value as u8);
    } else if value <= 1_073_741_823 {
        buf.put_u8(((value >> 24) as u8) | 0b1000_0000);
        buf.put_u8((value >> 16) as u8);
        buf.put_u8((value >> 8) as u8);
        buf.put_u8(value as u8);
    } else {
        buf.put_u8(((value >> 56) as u8) | 0b1100_0000);
        buf.put_u8((value >> 48) as u8);
        buf.put_u8((value >> 40) as u8);
        buf.put_u8((value >> 32) as u8);
        buf.put_u8((value >> 24) as u8);
        buf.put_u8((value >> 16) as u8);
        buf.put_u8((value >> 8) as u8);
        buf.put_u8(value as u8);
    }
}

pub fn decode_varint(buf: &mut Bytes) -> Result<u64> {
    if buf.remaining() < 1 {
        return Err(MqttError::ProtocolError(
            "insufficient data for varint".into(),
        ));
    }
    let first = buf.get_u8();
    let prefix = first >> 6;
    match prefix {
        0b00 => Ok((first & 0x3F) as u64),
        0b01 => {
            if buf.remaining() < 1 {
                return Err(MqttError::ProtocolError(
                    "insufficient data for 2-byte varint".into(),
                ));
            }
            let second = buf.get_u8();
            Ok((((first & 0x3F) as u64) << 8) | (second as u64))
        }
        0b10 => {
            if buf.remaining() < 3 {
                return Err(MqttError::ProtocolError(
                    "insufficient data for 4-byte varint".into(),
                ));
            }
            let b1 = buf.get_u8();
            let b2 = buf.get_u8();
            let b3 = buf.get_u8();
            Ok((((first & 0x3F) as u64) << 24)
                | ((b1 as u64) << 16)
                | ((b2 as u64) << 8)
                | (b3 as u64))
        }
        0b11 => {
            if buf.remaining() < 7 {
                return Err(MqttError::ProtocolError(
                    "insufficient data for 8-byte varint".into(),
                ));
            }
            let b1 = buf.get_u8();
            let b2 = buf.get_u8();
            let b3 = buf.get_u8();
            let b4 = buf.get_u8();
            let b5 = buf.get_u8();
            let b6 = buf.get_u8();
            let b7 = buf.get_u8();
            Ok((((first & 0x3F) as u64) << 56)
                | ((b1 as u64) << 48)
                | ((b2 as u64) << 40)
                | ((b3 as u64) << 32)
                | ((b4 as u64) << 24)
                | ((b5 as u64) << 16)
                | ((b6 as u64) << 8)
                | (b7 as u64))
        }
        _ => unreachable!(),
    }
}

pub fn varint_len(value: u64) -> usize {
    if value <= 63 {
        1
    } else if value <= 16383 {
        2
    } else if value <= 1_073_741_823 {
        4
    } else {
        8
    }
}

#[derive(Debug, Default)]
pub struct FlowIdGenerator {
    next_client: u64,
    next_server: u64,
}

impl FlowIdGenerator {
    pub fn new() -> Self {
        Self {
            next_client: 1,
            next_server: 1,
        }
    }

    pub fn next_client(&mut self) -> FlowId {
        let id = FlowId::client(self.next_client);
        self.next_client += 1;
        id
    }

    pub fn next_server(&mut self) -> FlowId {
        let id = FlowId::server(self.next_server);
        self.next_server += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_encode_decode_1byte() {
        let mut buf = BytesMut::new();
        encode_varint(0, &mut buf);
        assert_eq!(buf.len(), 1);
        let mut bytes = buf.freeze();
        assert_eq!(decode_varint(&mut bytes).unwrap(), 0);

        let mut buf = BytesMut::new();
        encode_varint(63, &mut buf);
        assert_eq!(buf.len(), 1);
        let mut bytes = buf.freeze();
        assert_eq!(decode_varint(&mut bytes).unwrap(), 63);
    }

    #[test]
    fn test_varint_encode_decode_2bytes() {
        let mut buf = BytesMut::new();
        encode_varint(64, &mut buf);
        assert_eq!(buf.len(), 2);
        let mut bytes = buf.freeze();
        assert_eq!(decode_varint(&mut bytes).unwrap(), 64);

        let mut buf = BytesMut::new();
        encode_varint(16383, &mut buf);
        assert_eq!(buf.len(), 2);
        let mut bytes = buf.freeze();
        assert_eq!(decode_varint(&mut bytes).unwrap(), 16383);
    }

    #[test]
    fn test_varint_encode_decode_4bytes() {
        let mut buf = BytesMut::new();
        encode_varint(16384, &mut buf);
        assert_eq!(buf.len(), 4);
        let mut bytes = buf.freeze();
        assert_eq!(decode_varint(&mut bytes).unwrap(), 16384);

        let mut buf = BytesMut::new();
        encode_varint(1_073_741_823, &mut buf);
        assert_eq!(buf.len(), 4);
        let mut bytes = buf.freeze();
        assert_eq!(decode_varint(&mut bytes).unwrap(), 1_073_741_823);
    }

    #[test]
    fn test_varint_encode_decode_8bytes() {
        let mut buf = BytesMut::new();
        encode_varint(1_073_741_824, &mut buf);
        assert_eq!(buf.len(), 8);
        let mut bytes = buf.freeze();
        assert_eq!(decode_varint(&mut bytes).unwrap(), 1_073_741_824);

        let max = 4_611_686_018_427_387_903u64;
        let mut buf = BytesMut::new();
        encode_varint(max, &mut buf);
        assert_eq!(buf.len(), 8);
        let mut bytes = buf.freeze();
        assert_eq!(decode_varint(&mut bytes).unwrap(), max);
    }

    #[test]
    fn test_flow_id_client() {
        let id = FlowId::client(1);
        assert!(id.is_client_initiated());
        assert!(!id.is_server_initiated());
        assert_eq!(id.sequence(), 1);
        assert_eq!(id.raw(), 2);
    }

    #[test]
    fn test_flow_id_server() {
        let id = FlowId::server(1);
        assert!(!id.is_client_initiated());
        assert!(id.is_server_initiated());
        assert_eq!(id.sequence(), 1);
        assert_eq!(id.raw(), 3);
    }

    #[test]
    fn test_flow_flags_encode_decode() {
        let flags = FlowFlags {
            clean: 1,
            abort_if_no_state: 0,
            err_tolerance: 2,
            persistent_qos: 1,
            persistent_topic_alias: 0,
            persistent_subscriptions: 1,
            optional_headers: 0,
        };
        let encoded = flags.encode();
        let decoded = FlowFlags::decode(encoded);
        assert_eq!(flags, decoded);
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn test_control_flow_header_encode_decode() {
        let header = ControlFlowHeader::new(FlowFlags::default());
        let mut buf = BytesMut::new();
        header.encode(&mut buf);

        let mut bytes = buf.freeze();
        let flow_type = decode_varint(&mut bytes).unwrap() as u8;
        assert_eq!(flow_type, FLOW_TYPE_CONTROL);

        let decoded = ControlFlowHeader::decode(&mut bytes).unwrap();
        assert_eq!(header.flow_id.raw(), decoded.flow_id.raw());
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn test_data_flow_header_encode_decode() {
        let flow_id = FlowId::client(42);
        let header = DataFlowHeader::client(
            flow_id,
            3600,
            FlowFlags {
                persistent_qos: 1,
                ..Default::default()
            },
        );
        let mut buf = BytesMut::new();
        header.encode(&mut buf);

        let mut bytes = buf.freeze();
        let flow_type = decode_varint(&mut bytes).unwrap() as u8;
        assert_eq!(flow_type, FLOW_TYPE_CLIENT_DATA);

        let decoded = DataFlowHeader::decode(flow_type, &mut bytes).unwrap();
        assert_eq!(decoded.flow_id.raw(), flow_id.raw());
        assert_eq!(decoded.expire_interval, 3600);
        assert_eq!(decoded.flags.persistent_qos, 1);
    }

    #[test]
    fn test_flow_header_enum() {
        let header = FlowHeader::Control(ControlFlowHeader::new(FlowFlags::default()));
        let mut buf = BytesMut::new();
        header.encode(&mut buf);

        let mut bytes = buf.freeze();
        let decoded = FlowHeader::decode(&mut bytes).unwrap();
        match decoded {
            FlowHeader::Control(h) => {
                assert_eq!(h.flow_id.raw(), 0);
            }
            _ => panic!("expected Control flow header"),
        }
    }

    #[test]
    fn test_flow_id_generator() {
        let mut gen = FlowIdGenerator::new();
        let c1 = gen.next_client();
        let c2 = gen.next_client();
        let s1 = gen.next_server();
        let s2 = gen.next_server();

        assert!(c1.is_client_initiated());
        assert!(c2.is_client_initiated());
        assert!(s1.is_server_initiated());
        assert!(s2.is_server_initiated());

        assert_eq!(c1.sequence(), 1);
        assert_eq!(c2.sequence(), 2);
        assert_eq!(s1.sequence(), 1);
        assert_eq!(s2.sequence(), 2);
    }
}
