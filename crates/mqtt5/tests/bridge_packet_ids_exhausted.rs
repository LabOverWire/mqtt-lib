#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use mqtt5_protocol::packet::publish::PublishPacket;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const IN_FLIGHT: usize = 65_535;

struct UnackingRemote {
    connections: AtomicUsize,
    first_publishes: AtomicUsize,
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

impl UnackingRemote {
    async fn start() -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            connections: AtomicUsize::new(0),
            first_publishes: AtomicUsize::new(0),
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
        let closes_at = (number == 2).then(|| tokio::time::Instant::now() + Duration::from_secs(3));
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16_384];
        loop {
            let read = match closes_at {
                Some(at) => match tokio::time::timeout_at(at, stream.read(&mut chunk)).await {
                    Ok(read) => read,
                    Err(_) => return,
                },
                None => stream.read(&mut chunk).await,
            };
            match read {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                let reply = match header >> 4 {
                    1 => Some(vec![0x20, 3, u8::from(number > 1), 0, 0]),
                    3 if number == 1 => {
                        if self.first_publishes.fetch_add(1, Ordering::SeqCst) + 1 >= IN_FLIGHT {
                            return;
                        }
                        None
                    }
                    8 => Some(vec![0x90, 4, body[0], body[1], 0, 1]),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridge_keeps_running_when_resent_publishes_hold_every_packet_id() {
    let (remote, addr) = UnackingRemote::start().await;
    let mut config = BridgeConfig::new("packet-ids-exhausted", addr.to_string())
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("out/#", BridgeDirection::Out, QoS::AtLeastOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    config.outbound_queue_capacity = 70_001;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let run = tokio::spawn(async move { running.run().await });

    for i in 0..70_000 {
        let packet = PublishPacket::new("out/x", format!("m{i}").into_bytes(), QoS::AtLeastOnce);
        bridge.forward_message(&packet);
    }

    let filled = tokio::time::Instant::now() + Duration::from_secs(180);
    while remote.first_publishes.load(Ordering::SeqCst) < IN_FLIGHT
        && tokio::time::Instant::now() < filled
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let in_flight = remote.first_publishes.load(Ordering::SeqCst);

    let third = tokio::time::Instant::now() + Duration::from_secs(30);
    while remote.connections.load(Ordering::SeqCst) < 3
        && !run.is_finished()
        && tokio::time::Instant::now() < third
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let connections = remote.connections.load(Ordering::SeqCst);
    let outcome = if run.is_finished() {
        Some(run.await.unwrap())
    } else {
        run.abort();
        None
    };
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;

    assert_eq!(
        in_flight, IN_FLIGHT,
        "setup: the first connection should hold every packet identifier"
    );
    assert!(
        connections >= 3 && outcome.is_none(),
        "the bridge stopped while resent publishes held every packet identifier: connections = {connections}, run() = {outcome:?}"
    );
}
