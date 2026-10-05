#![cfg(feature = "broker")]

use bytes::BytesMut;
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5::QoS;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use std::net::SocketAddr;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

const PINGREQ: [u8; 2] = [0xC0, 0x00];

struct Wire {
    stream: TcpStream,
    buffer: BytesMut,
}

impl Wire {
    async fn connect(addr: SocketAddr, client_id: &str) -> Option<Self> {
        let mut wire = Self {
            stream: TcpStream::connect(addr).await.ok()?,
            buffer: BytesMut::new(),
        };
        let mut connect = Vec::new();
        ConnectPacket::new(mqtt5_protocol::types::ConnectOptions::new(client_id))
            .encode(&mut connect)
            .expect("encode CONNECT");
        wire.stream.write_all(&connect).await.ok()?;
        match wire.next().await {
            Some(Packet::ConnAck(_)) => Some(wire),
            _ => None,
        }
    }

    async fn next(&mut self) -> Option<Packet> {
        tokio::time::timeout(
            Duration::from_secs(2),
            read_packet_from_stream(&mut self.stream, 5, &mut self.buffer, 1 << 20),
        )
        .await
        .ok()?
        .ok()
    }

    async fn pinged(&mut self) -> bool {
        self.stream.write_all(&PINGREQ).await.is_ok()
            && matches!(self.next().await, Some(Packet::PingResp))
    }
}

struct BrokerResponse {
    existing_client_pinged: bool,
    new_client_connected: bool,
}

async fn broker_response(abort_run: bool) -> BrokerResponse {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"))
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        });
    let mut broker = MqttBroker::with_config(config).await.expect("start broker");
    let addr = broker.local_addr().expect("broker address");
    let mut ready = broker.ready_receiver();
    let run = tokio::spawn(async move { broker.run().await });
    ready.wait_for(|&up| up).await.expect("broker ready");

    let mut existing = Wire::connect(addr, "abort-existing")
        .await
        .expect("client connects before the abort");

    if abort_run {
        run.abort();
        assert!(run.await.is_err_and(|error| error.is_cancelled()));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    BrokerResponse {
        existing_client_pinged: existing.pinged().await,
        new_client_connected: Wire::connect(addr, "abort-new").await.is_some(),
    }
}

#[tokio::test]
async fn running_broker_serves_existing_and_new_clients() {
    let response = broker_response(false).await;
    assert!(response.existing_client_pinged);
    assert!(response.new_client_connected);
}

#[tokio::test]
async fn aborting_run_stops_serving_existing_and_new_clients() {
    let response = broker_response(true).await;
    assert!(!response.existing_client_pinged);
    assert!(!response.new_client_connected);
}

async fn alive_tasks_once_settled_at(expected: usize) -> usize {
    let metrics = tokio::runtime::Handle::current().metrics();
    let mut alive = metrics.num_alive_tasks();
    for _ in 0..80 {
        if alive == expected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        alive = metrics.num_alive_tasks();
    }
    alive
}

#[tokio::test]
async fn aborting_run_leaves_no_broker_task_running() {
    let before = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    broker_response(true).await;
    assert_eq!(alive_tasks_once_settled_at(before).await, before);
}

fn bridged_broker_config() -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"))
        .with_storage(StorageConfig::new().with_persistence(false))
}

async fn tasks_left_after_bridged_broker_stops(abort_run: bool) -> (usize, usize) {
    let metrics = tokio::runtime::Handle::current().metrics();
    let mut remote = MqttBroker::with_config(bridged_broker_config())
        .await
        .expect("start remote broker");
    let remote_addr = remote.local_addr().expect("remote address");
    let mut remote_ready = remote.ready_receiver();
    let remote_shutdown = remote.shutdown_handle();
    let remote_run = tokio::spawn(async move { remote.run().await });
    remote_ready.wait_for(|&up| up).await.expect("remote ready");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let baseline = metrics.num_alive_tasks();

    let mut bridge = BridgeConfig::new("abort-bridge", remote_addr.to_string()).add_topic(
        "bridged/#",
        BridgeDirection::Both,
        QoS::AtLeastOnce,
    );
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    let mut config = bridged_broker_config();
    config.bridges = vec![bridge];
    let mut local = MqttBroker::with_config(config)
        .await
        .expect("start bridged broker");
    let mut local_ready = local.ready_receiver();
    let local_shutdown = local.shutdown_handle();
    let local_run = tokio::spawn(async move { local.run().await });
    local_ready
        .wait_for(|&up| up)
        .await
        .expect("bridged broker ready");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(metrics.num_alive_tasks() > baseline);

    if abort_run {
        local_run.abort();
    } else {
        local_shutdown.shutdown();
    }
    drop(local_run.await);
    let left = alive_tasks_once_settled_at(baseline).await;

    remote_shutdown.shutdown();
    drop(remote_run.await);
    (baseline, left)
}

#[tokio::test]
async fn graceful_shutdown_stops_the_bridge() {
    let (baseline, left) = tasks_left_after_bridged_broker_stops(false).await;
    assert_eq!(left, baseline);
}

#[tokio::test]
async fn aborting_run_stops_the_bridge() {
    let (baseline, left) = tasks_left_after_bridged_broker_stops(true).await;
    assert_eq!(left, baseline);
}
