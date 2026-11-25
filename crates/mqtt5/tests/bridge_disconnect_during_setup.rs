#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Script {
    HoldSubscribeDisconnectOnPublish,
    NormalThenClose,
    NoIdsDisconnectOnUnsubscribe,
    NoIds,
    Normal,
}

struct Remote {
    connections: AtomicUsize,
    log: Mutex<Vec<String>>,
    scripts: Vec<Script>,
    fallback: Script,
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

fn topics(body: &[u8], with_options: bool) -> Vec<String> {
    let (props_len, mut pos) = varint(body, 2).unwrap_or((0, 3));
    pos += props_len;
    let mut found = Vec::new();
    while pos + 2 <= body.len() {
        let len = usize::from(u16::from_be_bytes([body[pos], body[pos + 1]]));
        found.push(String::from_utf8_lossy(&body[pos + 2..pos + 2 + len]).to_string());
        pos += 2 + len + usize::from(with_options);
    }
    found
}

impl Remote {
    async fn start(scripts: Vec<Script>, fallback: Script) -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            connections: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
            scripts,
            fallback,
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = Arc::clone(&remote);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let index = serving.connections.fetch_add(1, Ordering::SeqCst);
                let script = serving
                    .scripts
                    .get(index)
                    .copied()
                    .unwrap_or(serving.fallback);
                tokio::spawn(Arc::clone(&serving).serve(stream, index, script));
            }
        });
        (remote, addr)
    }

    fn note(&self, entry: String) {
        self.log.lock().unwrap().push(entry);
    }

    fn entries(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    async fn serve(self: Arc<Self>, mut stream: TcpStream, index: usize, script: Script) {
        let no_ids = matches!(script, Script::NoIds | Script::NoIdsDisconnectOnUnsubscribe);
        let session_present = u8::from(index > 0);
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut subacks = 0usize;
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                let reply: Option<Vec<u8>> = match header >> 4 {
                    1 => {
                        if no_ids {
                            Some(vec![0x20, 5, session_present, 0, 2, 0x29, 0])
                        } else {
                            Some(vec![0x20, 3, session_present, 0, 0])
                        }
                    }
                    3 => {
                        self.note(format!("{index}: PUBLISH"));
                        if script == Script::HoldSubscribeDisconnectOnPublish {
                            let _ = stream.write_all(&[0xE0, 1, 0x87]).await;
                            let _ = stream.flush().await;
                            return;
                        }
                        None
                    }
                    8 => {
                        let filters = topics(&body, true);
                        self.note(format!("{index}: SUBSCRIBE {filters:?}"));
                        if script == Script::HoldSubscribeDisconnectOnPublish {
                            None
                        } else {
                            let mut reply = vec![0x90];
                            let len = u8::try_from(3 + filters.len()).unwrap();
                            reply.extend_from_slice(&[len, body[0], body[1], 0]);
                            reply.extend(std::iter::repeat_n(1u8, filters.len()));
                            subacks += 1;
                            Some(reply)
                        }
                    }
                    10 => {
                        let filters = topics(&body, false);
                        self.note(format!("{index}: UNSUBSCRIBE {filters:?}"));
                        if script == Script::NoIdsDisconnectOnUnsubscribe {
                            let _ = stream.write_all(&[0xE0, 1, 0x87]).await;
                            let _ = stream.flush().await;
                            return;
                        }
                        Some(vec![0xB0, 4, body[0], body[1], 0, 0])
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
                if script == Script::NormalThenClose && subacks == 3 {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    return;
                }
            }
        }
    }
}

fn tune(config: &mut BridgeConfig) {
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_refusing_a_forwarded_publish_during_setup_does_not_end_run() {
    let (remote, addr) = Remote::start(
        vec![Script::HoldSubscribeDisconnectOnPublish],
        Script::Normal,
    )
    .await;
    let mut config = BridgeConfig::new("publish-refused", addr.to_string())
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("out/#", BridgeDirection::Out, QoS::AtMostOnce);
    tune(&mut config);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    bridge.forward_message(&PublishPacket::new(
        "out/x",
        b"hello".to_vec(),
        QoS::AtMostOnce,
    ));
    let running = Arc::clone(&bridge);
    let run = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_secs(4)).await;
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
        "a DISCONNECT 0x87 the remote sent for a forwarded PUBLISH, while a SUBSCRIBE awaited its SUBACK, ended run() as a subscription refusal: outcome = {outcome:?}, connections = {connections}, log = {:?}",
        remote.entries()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribes_not_sent_after_a_refusal_coded_disconnect_are_sent_on_the_next_connection() {
    let (remote, addr) = Remote::start(
        vec![
            Script::NormalThenClose,
            Script::NoIdsDisconnectOnUnsubscribe,
        ],
        Script::NoIds,
    )
    .await;
    let mut config = BridgeConfig::new("unsubscribe-kept", addr.to_string())
        .add_topic("a/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("a/b", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("a/c", BridgeDirection::In, QoS::AtLeastOnce);
    tune(&mut config);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let run = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_secs(6)).await;
    let finished = run.is_finished();
    run.abort();
    let log = remote.entries();
    let connections = remote.connections.load(Ordering::SeqCst);
    let unsubscribed_c = log
        .iter()
        .any(|entry| entry.contains("UNSUBSCRIBE") && entry.contains("\"a/c\""));
    assert!(
        connections >= 3 && unsubscribed_c,
        "the bridge never sent UNSUBSCRIBE for a/c, a filter it subscribed and no longer wants: connections = {connections}, finished = {finished}, log = {log:?}"
    );
}
