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

struct RefusingRemote {
    connections: AtomicUsize,
    subscribes: AtomicUsize,
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

impl RefusingRemote {
    async fn start() -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            connections: AtomicUsize::new(0),
            subscribes: AtomicUsize::new(0),
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
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                match header >> 4 {
                    1 => {
                        if stream.write_all(&[0x20, 3, 0, 0, 0]).await.is_err() {
                            return;
                        }
                    }
                    8 => {
                        self.subscribes.fetch_add(1, Ordering::SeqCst);
                        let reply = [0x90, 4, body[0], body[1], 0, 0x87, 0xE0, 1, 0x87];
                        let _ = stream.write_all(&reply).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusal_followed_by_disconnect_ends_run() {
    let (remote, addr) = RefusingRemote::start().await;
    let mut config = BridgeConfig::new("refuse-close", addr.to_string()).add_topic(
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
    assert!(
        finished && outcome.as_ref().is_some_and(Result::is_err),
        "a remote that refused the filter (SUBACK 0x87) and then sent DISCONNECT 0x87 did not end run(): outcome = {outcome:?}, connections = {connections}, subscribes = {subscribes}"
    );
}
