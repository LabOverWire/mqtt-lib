use mqtt5::time::Duration;
use mqtt5::types::{ConnectOptions, PublishOptions};
use mqtt5::{MqttClient, QoS};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const NEVER: usize = usize::MAX;
const LATER_ACK_DELAY: Duration = Duration::from_millis(300);

struct FakeServer {
    close: AtomicUsize,
    acks: AtomicBool,
    connections: AtomicUsize,
    log: Mutex<Vec<(usize, String)>>,
}

fn split_packet(buf: &[u8]) -> Option<(u8, Vec<u8>, usize)> {
    let header = *buf.first()?;
    let (len, start) = varint(buf, 1)?;
    let end = start + len;
    (buf.len() >= end).then(|| (header, buf[start..end].to_vec(), end))
}

fn varint(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut value = 0usize;
    let mut shift = 0;
    let mut pos = from;
    loop {
        let byte = *buf.get(pos)?;
        value |= usize::from(byte & 0x7f) << shift;
        pos += 1;
        if byte & 0x80 == 0 {
            return Some((value, pos));
        }
        shift += 7;
    }
}

impl FakeServer {
    async fn start() -> (Arc<Self>, SocketAddr) {
        let server = Arc::new(Self {
            close: AtomicUsize::new(NEVER),
            acks: AtomicBool::new(true),
            connections: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = Arc::clone(&server);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let index = serving.connections.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(Arc::clone(&serving).serve(stream, index));
            }
        });
        (server, addr)
    }

    fn payloads(&self, connection: usize) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| *c == connection)
            .map(|(_, p)| p.clone())
            .collect()
    }

    async fn serve(self: Arc<Self>, mut stream: TcpStream, index: usize) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if self.close.load(Ordering::SeqCst) == index {
                return;
            }
            match tokio::time::timeout(Duration::from_millis(10), stream.read(&mut chunk)).await {
                Err(_) => continue,
                Ok(Ok(0) | Err(_)) => return,
                Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                let reply = match header >> 4 {
                    1 if index == 0 => Some(vec![0x20, 6, 0, 0, 3, 0x21, 0, 100]),
                    1 => Some(vec![0x20, 6, 1, 0, 3, 0x21, 0, 1]),
                    3 => {
                        let qos = (header >> 1) & 3;
                        let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                        let mut pos = 2 + topic_len;
                        let id = (qos > 0).then(|| [body[pos], body[pos + 1]]);
                        if qos > 0 {
                            pos += 2;
                        }
                        let (props, after) = varint(&body, pos).unwrap();
                        let payload = String::from_utf8_lossy(&body[after + props..]).to_string();
                        self.log.lock().unwrap().push((index, payload));
                        match id {
                            Some([hi, lo]) if index > 0 && self.acks.load(Ordering::SeqCst) => {
                                tokio::time::sleep(LATER_ACK_DELAY).await;
                                Some(vec![0x40, 2, hi, lo])
                            }
                            _ => None,
                        }
                    }
                    12 => Some(vec![0xD0, 0]),
                    14 => return,
                    _ => None,
                };
                if let Some(reply) = reply {
                    if stream.write_all(&reply).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

async fn wait_until(limit: Duration, check: impl Fn() -> bool) -> bool {
    let started = tokio::time::Instant::now();
    while started.elapsed() < limit {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    check()
}

fn options() -> ConnectOptions {
    ConnectOptions::new("qos0-after-resends")
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_automatic_reconnect(false)
}

async fn reconnected_with_two_resends(server: &FakeServer, addr: SocketAddr) -> MqttClient {
    let url = format!("mqtt://{addr}");
    let client = MqttClient::new("qos0-after-resends");
    client.connect_with_options(&url, options()).await.unwrap();

    let at_least_once = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    for payload in ["a1", "a2"] {
        assert!(
            client
                .begin_publish("a/x", payload, at_least_once.clone())
                .await
                .is_ok(),
            "setup: QoS 1 publish {payload} not started"
        );
    }
    assert!(
        wait_until(Duration::from_secs(2), || server.payloads(0).len() >= 2).await,
        "setup: QoS 1 messages not sent on the first connection"
    );

    server.close.store(0, Ordering::SeqCst);
    let mut down = false;
    for _ in 0..200 {
        if !client.is_connected().await {
            down = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(down, "setup: client never noticed the closed connection");

    client.connect_with_options(&url, options()).await.unwrap();
    client
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos0_published_after_reconnect_follows_the_qos1_resends() {
    let (server, addr) = FakeServer::start().await;
    let client = reconnected_with_two_resends(&server, addr).await;
    client.publish_qos0("b/x", "b1").await.unwrap();

    wait_until(Duration::from_secs(5), || server.payloads(1).len() >= 3).await;
    let delivered = server.payloads(1);
    let _ = client.disconnect().await;
    assert_eq!(
        delivered,
        vec!["a1", "a2", "b1"],
        "b1 was published after the reconnect, but the server received it before the QoS 1 resends were all sent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos0_waiting_for_resends_returns_when_the_connection_ends() {
    let (server, addr) = FakeServer::start().await;
    server.acks.store(false, Ordering::SeqCst);
    let client = Arc::new(reconnected_with_two_resends(&server, addr).await);
    let publishing = Arc::clone(&client);
    let waiting = tokio::spawn(async move { publishing.publish_qos0("b/x", "b1").await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stalled = server.payloads(1);
    server.close.store(1, Ordering::SeqCst);
    let returned = tokio::time::timeout(Duration::from_secs(5), waiting).await;
    let _ = client.disconnect().await;
    assert_eq!(
        stalled,
        vec!["a1"],
        "setup: the resends should stall at a1 with Receive Maximum 1 and no acknowledgement"
    );
    assert!(
        returned.is_ok(),
        "a QoS 0 publish waiting for the resends did not return after the connection ended"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos0_waiting_for_stalled_resends_gives_up_like_qos1() {
    let (server, addr) = FakeServer::start().await;
    server.acks.store(false, Ordering::SeqCst);
    let client = Arc::new(reconnected_with_two_resends(&server, addr).await);
    let qos1_client = Arc::clone(&client);
    let qos1 = tokio::spawn(async move {
        qos1_client
            .publish_with_options(
                "c/x",
                "c1",
                PublishOptions {
                    qos: QoS::AtLeastOnce,
                    ..Default::default()
                },
            )
            .await
    });
    let qos0_client = Arc::clone(&client);
    let qos0 = tokio::spawn(async move { qos0_client.publish_qos0("b/x", "b1").await });

    let qos1_outcome = tokio::time::timeout(Duration::from_secs(40), qos1).await;
    let qos0_outcome = tokio::time::timeout(Duration::from_secs(5), qos0).await;
    let still_connected = client.is_connected().await;
    let sent = server.payloads(1);
    let _ = client.disconnect().await;

    assert!(still_connected, "setup: the connection should stay up");
    assert!(
        matches!(
            qos1_outcome,
            Ok(Ok(Err(mqtt5::MqttError::FlowControlExceeded)))
        ),
        "setup: the QoS 1 publish should give up with FlowControlExceeded, got {qos1_outcome:?}"
    );
    assert!(
        matches!(
            qos0_outcome,
            Ok(Ok(Err(mqtt5::MqttError::FlowControlExceeded)))
        ),
        "a QoS 0 publish waiting for stalled resends should give up with FlowControlExceeded like the QoS 1 publish, got {qos0_outcome:?}"
    );
    assert!(
        !sent.iter().any(|payload| payload == "b1"),
        "the QoS 0 publish that gave up was sent anyway: {sent:?}"
    );
}
