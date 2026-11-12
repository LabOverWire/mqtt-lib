#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use mqtt5_protocol::packet::publish::PublishPacket;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct PublishDenyingRemote {
    answer_subscribe_while_denying: bool,
    connections: AtomicUsize,
    log: Mutex<Vec<(usize, u8)>>,
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

fn split_packet(buf: &[u8]) -> Option<(u8, Vec<u8>, usize)> {
    let header = *buf.first()?;
    let (len, start) = varint(buf, 1)?;
    let end = start + len;
    (buf.len() >= end).then(|| (header, buf[start..end].to_vec(), end))
}

impl PublishDenyingRemote {
    async fn start(answer_subscribe_while_denying: bool) -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            answer_subscribe_while_denying,
            connections: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = Arc::clone(&remote);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let number = serving.connections.fetch_add(1, Ordering::SeqCst) + 1;
                tokio::spawn(Arc::clone(&serving).serve(stream, number));
            }
        });
        (remote, addr)
    }

    async fn serve(self: Arc<Self>, mut stream: TcpStream, number: usize) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                self.log.lock().unwrap().push((number, header));
                match header >> 4 {
                    1 => {
                        let session_present = u8::from(number > 1);
                        if stream
                            .write_all(&[0x20, 3, session_present, 0, 0])
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    3 => match number {
                        1 => return,
                        2 => {
                            let _ = stream.write_all(&[0xE0, 1, 0x87]).await;
                            let _ = stream.flush().await;
                            return;
                        }
                        _ => {
                            let topic_len = usize::from(body[0]) << 8 | usize::from(body[1]);
                            let pid = &body[2 + topic_len..4 + topic_len];
                            if stream.write_all(&[0x40, 2, pid[0], pid[1]]).await.is_err() {
                                return;
                            }
                        }
                    },
                    8 => {
                        if number == 2 && !self.answer_subscribe_while_denying {
                            continue;
                        }
                        let ack = [0x90, 4, body[0], body[1], 0, 1];
                        if stream.write_all(&ack).await.is_err() {
                            return;
                        }
                    }
                    12 => {
                        if stream.write_all(&[0xD0, 0]).await.is_err() {
                            return;
                        }
                    }
                    14 => return,
                    _ => {}
                }
            }
        }
    }
}

async fn observe(answer_subscribe_while_denying: bool, in_filters: &[&str]) {
    let (remote, addr) = PublishDenyingRemote::start(answer_subscribe_while_denying).await;
    let mut config = BridgeConfig::new("resend", addr.to_string()).add_topic(
        "out/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    for filter in in_filters {
        config = config.add_topic(*filter, BridgeDirection::In, QoS::AtLeastOnce);
    }
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let run = tokio::spawn(async move { running.run().await });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !bridge.get_stats().await.connected {
        assert!(tokio::time::Instant::now() < deadline, "never connected");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    bridge.forward_message(&PublishPacket::new("out/x", &b"m"[..], QoS::AtLeastOnce));

    tokio::time::sleep(Duration::from_secs(6)).await;
    let finished = run.is_finished();
    let connections = remote.connections.load(Ordering::SeqCst);
    let log = remote.log.lock().unwrap().clone();
    let outcome = if finished {
        Some(run.await.unwrap())
    } else {
        run.abort();
        None
    };
    assert!(
        !finished && connections >= 3,
        "a DISCONNECT 0x87 answering a resent PUBLISH during setup ended run() as a subscription refusal: outcome = {outcome:?}, connections = {connections}, packets (connection, header) = {log:02X?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resent_publish_denied_by_disconnect_during_setup_does_not_end_run() {
    observe(false, &["in/#"]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resent_publish_denied_by_disconnect_between_subscribes_does_not_end_run() {
    observe(true, &["in/a/#", "in/b/#", "in/c/#"]).await;
}
