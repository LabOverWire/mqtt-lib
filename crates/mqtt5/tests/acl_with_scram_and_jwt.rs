#![cfg(feature = "broker")]

use base64::prelude::*;
use mqtt5::broker::auth::PasswordAuthProvider;
use mqtt5::broker::auth_mechanisms::generate_scram_credential_line_with_iterations;
use mqtt5::broker::config::{
    AuthConfig, BrokerConfig, JwtAlgorithm, JwtConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::ConnectOptions;
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::subscribe::SubscribePacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::QoS;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const SECRET: &str = "acl-test-hs256-secret-0123456789abcdef";
const ACL: &str = "user writer topic secret/# permission readwrite\n\
                   user * topic secret/# permission deny\n\
                   user * topic # permission readwrite\n";

struct Raw {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Raw {
    async fn open(addr: SocketAddr) -> Self {
        Self {
            stream: TcpStream::connect(addr).await.unwrap(),
            buf: Vec::new(),
        }
    }

    async fn send<P: MqttPacket>(&mut self, packet: &P) {
        let mut bytes = bytes::BytesMut::new();
        packet.encode(&mut bytes).unwrap();
        self.stream.write_all(&bytes).await.unwrap();
    }

    async fn frames(&mut self, wait_ms: u64) -> Vec<Vec<u8>> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        let mut chunk = [0u8; 4096];
        while let Ok(Ok(n)) = tokio::time::timeout_at(deadline, self.stream.read(&mut chunk)).await
        {
            if n == 0 {
                break;
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
        let mut out = Vec::new();
        let mut at = 0;
        while at < self.buf.len() {
            let mut len = 0usize;
            let mut shift = 0;
            let mut i = at + 1;
            loop {
                let b = self.buf[i];
                len |= usize::from(b & 0x7f) << shift;
                shift += 7;
                i += 1;
                if b & 0x80 == 0 {
                    break;
                }
            }
            out.push(self.buf[at..i + len].to_vec());
            at = i + len;
        }
        self.buf.clear();
        out
    }
}

fn body(frame: &[u8]) -> &[u8] {
    let mut i = 1;
    while frame[i] & 0x80 != 0 {
        i += 1;
    }
    &frame[i + 1..]
}

fn describe(frames: &[Vec<u8>]) -> String {
    frames
        .iter()
        .map(|f| match f[0] >> 4 {
            2 => format!("CONNACK(rc=0x{:02x})", body(f)[1]),
            3 => {
                let b = body(f);
                let tlen = usize::from(b[0]) << 8 | usize::from(b[1]);
                format!(
                    "PUBLISH(topic={})",
                    String::from_utf8_lossy(&b[2..2 + tlen])
                )
            }
            4 => {
                let b = body(f);
                let rc = if b.len() > 2 { b[2] } else { 0 };
                format!("PUBACK(rc=0x{rc:02x})")
            }
            9 => {
                let b = body(f);
                let plen = usize::from(b[2]);
                format!("SUBACK(rc=0x{:02x})", b[3 + plen])
            }
            15 => "AUTH".to_string(),
            14 => format!("DISCONNECT({:02x?})", body(f)),
            t => format!("type{t}"),
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn base_config(auth: AuthConfig) -> BrokerConfig {
    let mut config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: false,
            ..Default::default()
        });
    config.auth_config = auth;
    config
}

async fn start(auth: AuthConfig) -> SocketAddr {
    let mut broker = MqttBroker::with_config(base_config(auth)).await.unwrap();
    let addr = broker.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    addr
}

fn b64url(data: &[u8]) -> String {
    BASE64_URL_SAFE_NO_PAD.encode(data)
}

fn jwt(sub: &str) -> String {
    let header = b64url(br#"{"alg":"HS256","typ":"JWT"}"#);
    let claims = b64url(format!(r#"{{"sub":"{sub}","exp":9999999999}}"#).as_bytes());
    let message = format!("{header}.{claims}");
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, SECRET.as_bytes());
    let sig = b64url(ring::hmac::sign(&key, message.as_bytes()).as_ref());
    format!("{message}.{sig}")
}

fn write_acl(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("acl.txt");
    std::fs::write(&path, ACL).unwrap();
    path
}

#[derive(Clone, Copy, Debug)]
enum Method {
    Password,
    Jwt,
    Scram,
}

fn auth_config(method: Method, dir: &Path) -> AuthConfig {
    let acl = write_acl(dir);
    match method {
        Method::Password => {
            let path = dir.join("passwd.txt");
            let lines = format!(
                "reader:{}\nwriter:{}\n",
                PasswordAuthProvider::hash_password("pw-r").unwrap(),
                PasswordAuthProvider::hash_password("pw-w").unwrap()
            );
            std::fs::write(&path, lines).unwrap();
            AuthConfig::new()
                .with_password_file(path)
                .with_acl_file(acl)
                .with_allow_anonymous(false)
        }
        Method::Jwt => {
            let key = dir.join("jwt.key");
            std::fs::write(&key, SECRET).unwrap();
            AuthConfig::new()
                .with_jwt(JwtConfig::new(JwtAlgorithm::HS256, key))
                .with_acl_file(acl)
                .with_allow_anonymous(false)
        }
        Method::Scram => {
            let path = dir.join("scram.txt");
            let lines = format!(
                "{}\n{}\n",
                generate_scram_credential_line_with_iterations("reader", "pw-r", 4096).unwrap(),
                generate_scram_credential_line_with_iterations("writer", "pw-w", 4096).unwrap()
            );
            std::fs::write(&path, lines).unwrap();
            AuthConfig::new()
                .with_scram_file(path)
                .with_acl_file(acl)
                .with_allow_anonymous(false)
        }
    }
}

fn server_first(frame: &[u8]) -> String {
    for p in 0..frame.len().saturating_sub(5) {
        if frame[p] == 0x16 && &frame[p + 3..p + 5] == b"r=" {
            let len = usize::from(frame[p + 1]) << 8 | usize::from(frame[p + 2]);
            return String::from_utf8_lossy(&frame[p + 3..p + 3 + len]).into_owned();
        }
    }
    panic!("no server-first in {frame:?}");
}

fn scram_final(user: &str, password: &str, server_first: &str) -> String {
    let mut nonce = "";
    let mut salt = Vec::new();
    let mut iterations = 0u32;
    for part in server_first.split(',') {
        if let Some(v) = part.strip_prefix("r=") {
            nonce = v;
        } else if let Some(v) = part.strip_prefix("s=") {
            salt = BASE64_STANDARD.decode(v).unwrap();
        } else if let Some(v) = part.strip_prefix("i=") {
            iterations = v.parse().unwrap();
        }
    }
    let mut salted = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(iterations).unwrap(),
        &salt,
        password.as_bytes(),
        &mut salted,
    );
    let hmac = |key: &[u8], data: &[u8]| {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
        ring::hmac::sign(&key, data).as_ref().to_vec()
    };
    let client_key = hmac(&salted, b"Client Key");
    let stored_key = ring::digest::digest(&ring::digest::SHA256, &client_key);
    let without_proof = format!("c=biws,r={nonce}");
    let auth_message = format!("n={user},r=clientnonce,{server_first},{without_proof}");
    let signature = hmac(stored_key.as_ref(), auth_message.as_bytes());
    let proof: Vec<u8> = client_key
        .iter()
        .zip(signature.iter())
        .map(|(a, b)| a ^ b)
        .collect();
    format!("{without_proof},p={}", BASE64_STANDARD.encode(proof))
}

async fn login(addr: SocketAddr, method: Method, id: &str, user: &str, pw: &str) -> (Raw, String) {
    let mut raw = Raw::open(addr).await;
    let options = ConnectOptions::new(id).with_clean_start(true);
    match method {
        Method::Password => {
            raw.send(&ConnectPacket::new(
                options.with_credentials(user, pw).protocol_options,
            ))
            .await;
        }
        Method::Jwt => {
            raw.send(&ConnectPacket::new(
                options
                    .with_authentication_method("JWT")
                    .with_authentication_data(jwt(user))
                    .protocol_options,
            ))
            .await;
        }
        Method::Scram => {
            raw.send(&ConnectPacket::new(
                options
                    .with_authentication_method("SCRAM-SHA-256")
                    .with_authentication_data(format!("n,,n={user},r=clientnonce"))
                    .protocol_options,
            ))
            .await;
            let frames = raw.frames(500).await;
            let first = server_first(&frames[0]);
            let mut auth = AuthPacket::new(ReasonCode::ContinueAuthentication);
            auth.properties
                .set_authentication_method("SCRAM-SHA-256".to_string());
            auth.properties
                .set_authentication_data(scram_final(user, pw, &first).into_bytes().into());
            raw.send(&auth).await;
        }
    }
    let frames = raw.frames(500).await;
    let connack = describe(&frames);
    (raw, connack)
}

async fn run_case(method: Method) -> [String; 5] {
    let dir = tempfile::tempdir().unwrap();
    let addr = start(auth_config(method, dir.path())).await;

    let (mut reader, reader_connack) = login(addr, method, "acl-reader", "reader", "pw-r").await;
    let (mut writer, writer_connack) = login(addr, method, "acl-writer", "writer", "pw-w").await;

    reader
        .send(&SubscribePacket::new(1).add_filter("secret/x", QoS::AtLeastOnce))
        .await;
    let sub_secret = describe(&reader.frames(400).await);

    reader
        .send(&SubscribePacket::new(2).add_filter("#", QoS::AtLeastOnce))
        .await;
    let sub_all = describe(&reader.frames(400).await);

    reader
        .send(
            &PublishPacket::new("secret/x", b"from-reader".as_ref(), QoS::AtLeastOnce)
                .with_packet_id(3),
        )
        .await;
    let reader_pub = describe(&reader.frames(400).await);

    writer
        .send(
            &PublishPacket::new("secret/x", b"from-writer".as_ref(), QoS::AtLeastOnce)
                .with_packet_id(4),
        )
        .await;
    let writer_pub = describe(&writer.frames(400).await);
    let reader_delivery = describe(&reader.frames(600).await);

    let lines = [
        format!("connack reader={reader_connack} writer={writer_connack}"),
        format!("reader SUBSCRIBE secret/x -> {sub_secret}"),
        format!("reader SUBSCRIBE # -> {sub_all}"),
        format!("reader PUBLISH secret/x -> {reader_pub}"),
        format!("writer PUBLISH secret/x -> {writer_pub}; reader received -> [{reader_delivery}]"),
    ];
    lines
}

fn assert_enforced(method: Method, lines: &[String; 5]) {
    assert!(
        lines[0].contains("CONNACK(rc=0x00)") && !lines[0].contains("rc=0x8"),
        "[{method:?}] login failed: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("SUBACK(rc=0x87)"),
        "[{method:?}] SUBSCRIBE secret/x not refused: {}",
        lines[1]
    );
    assert!(
        lines[3].contains("PUBACK(rc=0x87)"),
        "[{method:?}] PUBLISH secret/x not refused: {}",
        lines[3]
    );
    assert!(
        !lines[4].contains("PUBLISH(topic=secret/x)"),
        "[{method:?}] secret/x delivered to reader: {}",
        lines[4]
    );
}

#[tokio::test]
async fn password_authentication_enforces_the_acl_file() {
    let lines = run_case(Method::Password).await;
    assert_enforced(Method::Password, &lines);
}

#[tokio::test]
async fn jwt_authentication_enforces_the_acl_file() {
    let lines = run_case(Method::Jwt).await;
    assert_enforced(Method::Jwt, &lines);
}

#[tokio::test]
async fn scram_authentication_enforces_the_acl_file() {
    let lines = run_case(Method::Scram).await;
    assert_enforced(Method::Scram, &lines);
}
