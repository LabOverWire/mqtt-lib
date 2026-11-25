#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    SilentOnSubscribe,
    ResetAfterConnack,
}

struct FakeRemote {
    connections: AtomicUsize,
    subscribes: AtomicUsize,
    suback_code: AtomicU8,
    special_connections: usize,
    special: Mode,
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

fn subscribe_filter_count(body: &[u8]) -> usize {
    let (props_len, mut pos) = varint(body, 2).unwrap_or((0, 3));
    pos += props_len;
    let mut count = 0;
    while pos + 2 <= body.len() {
        let len = usize::from(u16::from_be_bytes([body[pos], body[pos + 1]]));
        pos += 2 + len + 1;
        count += 1;
    }
    count
}

impl FakeRemote {
    async fn start(special: Mode, special_connections: usize) -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            connections: AtomicUsize::new(0),
            subscribes: AtomicUsize::new(0),
            suback_code: AtomicU8::new(0),
            special_connections,
            special,
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = Arc::clone(&remote);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let index = serving.connections.fetch_add(1, Ordering::SeqCst);
                let mode = if index < serving.special_connections {
                    serving.special
                } else {
                    Mode::Normal
                };
                tokio::spawn(Arc::clone(&serving).serve(stream, mode));
            }
        });
        (remote, addr)
    }

    async fn serve(self: Arc<Self>, mut stream: TcpStream, mode: Mode) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                let reply: Option<Vec<u8>> = match header >> 4 {
                    1 => {
                        if mode == Mode::ResetAfterConnack {
                            let _ = stream.write_all(&[0x20, 3, 0, 0, 0]).await;
                            let _ = stream.flush().await;
                            let _ = stream.set_zero_linger();
                            drop(stream);
                            return;
                        }
                        Some(vec![0x20, 3, 0, 0, 0])
                    }
                    8 => {
                        self.subscribes.fetch_add(1, Ordering::SeqCst);
                        if mode == Mode::SilentOnSubscribe {
                            None
                        } else {
                            let count = subscribe_filter_count(&body);
                            let mut reply = vec![0x90];
                            let len = u8::try_from(3 + count).unwrap();
                            reply.extend_from_slice(&[len, body[0], body[1], 0]);
                            reply.extend(std::iter::repeat_n(
                                self.suback_code.load(Ordering::SeqCst),
                                count,
                            ));
                            Some(reply)
                        }
                    }
                    10 => Some(vec![0xB0, 3, body[0], body[1], 0]),
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

fn config(name: &str, addr: SocketAddr) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, addr.to_string())
        .add_topic("in/a/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("in/b/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("in/c/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("in/d/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rerun_after_refused_subscription_subscribes_again() {
    let (remote, addr) = FakeRemote::start(Mode::Normal, 0).await;
    remote.suback_code.store(0x87, Ordering::SeqCst);
    let bridge = Arc::new(
        BridgeConnection::new(config("rerun", addr), Arc::new(MessageRouter::new())).unwrap(),
    );
    bridge.start();
    let running = Arc::clone(&bridge);
    let result = tokio::time::timeout(Duration::from_secs(5), async move { running.run().await })
        .await
        .expect("setup: run() did not return");
    assert!(result.is_err(), "setup: run() did not fail on refusal");
    remote.suback_code.store(0, Ordering::SeqCst);
    let before = remote.subscribes.load(Ordering::SeqCst);

    bridge.start();
    let running = Arc::clone(&bridge);
    let rerun = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = remote.subscribes.load(Ordering::SeqCst);
    let connections = remote.connections.load(Ordering::SeqCst);
    let finished = rerun.is_finished();
    rerun.abort();
    assert!(
        after > before,
        "run() again after a refused subscription sent no SUBSCRIBE: subscribes before = {before}, after = {after}, connections = {connections}, rerun finished = {finished}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_silent_during_subscribe_does_not_stop_the_bridge() {
    let (remote, addr) = FakeRemote::start(Mode::SilentOnSubscribe, 1).await;
    let bridge = Arc::new(
        BridgeConnection::new(config("silent", addr), Arc::new(MessageRouter::new())).unwrap(),
    );
    bridge.start();
    let running = Arc::clone(&bridge);
    let run = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_secs(14)).await;
    let finished = run.is_finished();
    let connections = remote.connections.load(Ordering::SeqCst);
    let outcome = if finished {
        Some(run.await.unwrap())
    } else {
        run.abort();
        None
    };
    assert!(
        !finished,
        "run() ended while the remote only stopped answering a SUBSCRIBE: {outcome:?}, connections = {connections}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_after_connack_does_not_stop_the_bridge() {
    let (remote, addr) = FakeRemote::start(Mode::ResetAfterConnack, 20).await;
    let bridge = Arc::new(
        BridgeConnection::new(config("reset", addr), Arc::new(MessageRouter::new())).unwrap(),
    );
    bridge.start();
    let running = Arc::clone(&bridge);
    let run = tokio::spawn(async move { running.run().await });
    for _ in 0..200 {
        if run.is_finished() || remote.connections.load(Ordering::SeqCst) > 20 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let finished = run.is_finished();
    let connections = remote.connections.load(Ordering::SeqCst);
    let outcome = if finished {
        Some(run.await.unwrap())
    } else {
        run.abort();
        None
    };
    assert!(
        !finished,
        "run() ended after a remote reset the connection right after CONNACK: {outcome:?}, connections = {connections}"
    );
}
