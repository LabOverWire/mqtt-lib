#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct DisconnectingRemote {
    connections: AtomicUsize,
    subscribes: AtomicUsize,
    reason_code: u8,
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

fn split_packet(buf: &[u8]) -> Option<(u8, usize)> {
    let header = *buf.first()?;
    let (len, start) = varint(buf, 1)?;
    let end = start + len;
    (buf.len() >= end).then_some((header, end))
}

impl DisconnectingRemote {
    async fn start(reason_code: u8) -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            connections: AtomicUsize::new(0),
            subscribes: AtomicUsize::new(0),
            reason_code,
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = Arc::clone(&remote);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                serving.connections.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(Arc::clone(&serving).serve(stream));
            }
        });
        (remote, addr)
    }

    async fn serve(self: Arc<Self>, mut stream: TcpStream) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some((header, used)) = split_packet(&buf) {
                buf.drain(..used);
                match header >> 4 {
                    1 => {
                        if stream.write_all(&[0x20, 3, 0, 0, 0]).await.is_err() {
                            return;
                        }
                    }
                    8 => {
                        self.subscribes.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.write_all(&[0xE0, 1, self.reason_code]).await;
                        let _ = stream.flush().await;
                        return;
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

async fn run_against(reason_code: u8) {
    let (finished, outcome, connections, subscribes) = observe(reason_code).await;
    assert!(
        finished && outcome.as_ref().is_some_and(Result::is_err),
        "a remote that refused the SUBSCRIBE with DISCONNECT 0x{reason_code:02X} did not end run(): outcome = {outcome:?}, connections = {connections}, subscribes = {subscribes}"
    );
}

type Observed = (
    bool,
    Option<mqtt5::broker::bridge::Result<()>>,
    usize,
    usize,
);

async fn observe(reason_code: u8) -> Observed {
    let (remote, addr) = DisconnectingRemote::start(reason_code).await;
    let mut config = BridgeConfig::new("refuse-by-disconnect", addr.to_string()).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let run = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_secs(6)).await;
    let finished = run.is_finished();
    let connections = remote.connections.load(Ordering::SeqCst);
    let subscribes = remote.subscribes.load(Ordering::SeqCst);
    let outcome = if finished {
        Some(run.await.unwrap())
    } else {
        run.abort();
        None
    };
    (finished, outcome, connections, subscribes)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_refused_by_disconnect_wildcards_not_supported_ends_run() {
    run_against(0xA2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_refused_by_disconnect_topic_filter_invalid_ends_run() {
    run_against(0x8F).await;
}

async fn reconnects_after(reason_code: u8) {
    let (finished, outcome, connections, subscribes) = observe(reason_code).await;
    assert!(
        !finished && connections >= 3,
        "a remote that went away with DISCONNECT 0x{reason_code:02X} while subscribing ended run(): outcome = {outcome:?}, connections = {connections}, subscribes = {subscribes}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_ended_by_not_authorized_disconnect_reconnects() {
    reconnects_after(0x87).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_ended_by_server_shutting_down_reconnects() {
    reconnects_after(0x8B).await;
}
