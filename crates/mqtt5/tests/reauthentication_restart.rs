#![cfg(feature = "broker")]

use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult, EnhancedAuthStatus};
use mqtt5::broker::config::BrokerConfig;
use mqtt5::broker::MqttBroker;
use mqtt5::ConnectOptions;
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct TwoStepReauth;

impl AuthProvider for TwoStepReauth {
    fn authenticate<'a>(
        &'a self,
        _c: &'a ConnectPacket,
        _a: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = mqtt5::Result<AuthResult>> + Send + 'a>> {
        Box::pin(async { Ok(AuthResult::fail(ReasonCode::BadAuthenticationMethod)) })
    }
    fn authorize_publish<'a>(
        &'a self,
        _c: &str,
        _u: Option<&'a str>,
        _t: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn authorize_subscribe<'a>(
        &'a self,
        _c: &str,
        _u: Option<&'a str>,
        _t: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn supports_enhanced_auth(&self) -> bool {
        true
    }
    fn authenticate_enhanced<'a>(
        &'a self,
        m: &'a str,
        _d: Option<&'a [u8]>,
        _c: &'a str,
    ) -> Pin<Box<dyn Future<Output = mqtt5::Result<EnhancedAuthResult>> + Send + 'a>> {
        let m = m.to_string();
        Box::pin(async move { Ok(EnhancedAuthResult::success_with_user(m, "u".to_string())) })
    }
    fn reauthenticate<'a>(
        &'a self,
        m: &'a str,
        _d: Option<&'a [u8]>,
        _c: &'a str,
        _u: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = mqtt5::Result<EnhancedAuthResult>> + Send + 'a>> {
        let m = m.to_string();
        Box::pin(async move {
            let mut r = EnhancedAuthResult::success_with_user(m, "u".to_string());
            r.status = EnhancedAuthStatus::Continue;
            Ok(r)
        })
    }
}

async fn frames(stream: &mut TcpStream, wait_ms: u64) -> (Vec<u8>, bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut closed = false;
    loop {
        match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(0) | Err(_)) => {
                closed = true;
                break;
            }
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    (buf, closed)
}

async fn send<P: MqttPacket>(stream: &mut TcpStream, p: &P) {
    let mut b = bytes::BytesMut::new();
    p.encode(&mut b).unwrap();
    stream.write_all(&b).await.unwrap();
}

#[tokio::test]
async fn a_reauthentication_started_again_during_one_starts_over() {
    let config =
        BrokerConfig::default().with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());
    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(Arc::new(TwoStepReauth));
    let addr = broker.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut s = TcpStream::connect(addr).await.unwrap();
    let connect = ConnectPacket::new(
        ConnectOptions::new("rr")
            .with_authentication_method("M")
            .with_authentication_data(b"x")
            .protocol_options,
    );
    send(&mut s, &connect).await;
    let (connack, _) = frames(&mut s, 300).await;
    assert_eq!(connack[0], 0x20);
    let reauth = AuthPacket::re_authenticate("M".to_string(), Some(b"x".to_vec())).unwrap();
    send(&mut s, &reauth).await;
    let (first, _) = frames(&mut s, 300).await;
    assert_eq!(first[..3], [0xF0, 0x06, 0x18]);
    send(&mut s, &reauth).await;
    let (second, closed) = frames(&mut s, 500).await;
    assert_eq!(second[..3], [0xF0, 0x06, 0x18]);
    assert!(!closed);
}
