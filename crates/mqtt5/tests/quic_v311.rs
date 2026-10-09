#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::{BrokerShutdownHandle, MqttBroker};
use mqtt5::error::Result;
use mqtt5::transport::StreamStrategy;
use mqtt5::{ConnectOptions, MqttClient, ProtocolVersion, PublishOptions, QoS};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;
use tokio::task::JoinHandle;

const CERT: &str = "../../test_certs/server.pem";
const KEY: &str = "../../test_certs/server.key";
const SHORT_PAYLOAD: &[u8] = b"m";
const LONG_PAYLOAD: &[u8] = &[b'm'; 64];

static CRYPTO_PROVIDER: Once = Once::new();

struct Broker {
    addr: SocketAddr,
    shutdown: BrokerShutdownHandle,
    run: JoinHandle<Result<()>>,
}

impl Broker {
    async fn start() -> Self {
        CRYPTO_PROVIDER.call_once(|| {
            rustls::crypto::ring::default_provider()
                .install_default()
                .expect("install crypto provider");
        });
        let config = BrokerConfig::default()
            .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_quic(
                BrokerQuicConfig::new(PathBuf::from(CERT), PathBuf::from(KEY))
                    .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address")),
            );
        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("broker starts");
        let addr = broker.quic_local_addr().expect("QUIC endpoint bound");
        let shutdown = broker.shutdown_handle();
        let run = tokio::spawn(async move { broker.run().await });
        Self {
            addr,
            shutdown,
            run,
        }
    }

    async fn stop(self) {
        self.shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(5), self.run)
            .await
            .expect("broker stops")
            .expect("broker task joins")
            .expect("broker run succeeds");
    }
}

#[derive(Clone, Copy)]
struct Side {
    version: ProtocolVersion,
    strategy: StreamStrategy,
    datagrams: bool,
}

impl Side {
    const fn control(version: ProtocolVersion) -> Self {
        Self {
            version,
            strategy: StreamStrategy::ControlOnly,
            datagrams: false,
        }
    }

    const fn data_streams(version: ProtocolVersion) -> Self {
        Self {
            version,
            strategy: StreamStrategy::DataPerTopic,
            datagrams: false,
        }
    }

    const fn datagrams(version: ProtocolVersion) -> Self {
        Self {
            version,
            strategy: StreamStrategy::ControlOnly,
            datagrams: true,
        }
    }

    async fn connect(self, broker: &Broker, client_id: &str) -> MqttClient {
        let options = ConnectOptions::new(client_id).with_protocol_version(self.version);
        let client = MqttClient::with_options(options.clone());
        client.set_insecure_tls(true).await;
        client.set_quic_stream_strategy(self.strategy).await;
        client.set_quic_datagrams(self.datagrams).await;
        Box::pin(client.connect_with_options(&format!("quic://{}", broker.addr), options))
            .await
            .expect("client connects over QUIC");
        client
    }
}

async fn delivered(publisher: Side, subscriber: Side, qos: QoS, payload: &[u8]) -> bool {
    let broker = Broker::start().await;
    let receiving = subscriber.connect(&broker, "v311-sub").await;
    let received = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&received);
    let subscription_acked = receiving
        .subscribe("v311/t", move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .await
        .is_ok();
    let sending = publisher.connect(&broker, "v311-pub").await;
    let publish_completed = sending
        .publish_with_options(
            "v311/t",
            payload.to_vec(),
            PublishOptions {
                qos,
                ..Default::default()
            },
        )
        .await
        .is_ok();
    let mut arrived = false;
    for _ in 0..40 {
        if received.load(Ordering::SeqCst) > 0 {
            arrived = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    sending.disconnect().await.ok();
    receiving.disconnect().await.ok();
    broker.stop().await;
    subscription_acked && publish_completed && arrived
}

#[tokio::test]
async fn publish_on_a_data_stream_is_delivered() {
    for version in [ProtocolVersion::V5, ProtocolVersion::V311] {
        assert!(
            delivered(
                Side::data_streams(version),
                Side::control(ProtocolVersion::V5),
                QoS::AtLeastOnce,
                SHORT_PAYLOAD,
            )
            .await,
            "{version:?}"
        );
    }
}

#[tokio::test]
async fn subscribe_on_a_data_stream_is_delivered() {
    for version in [ProtocolVersion::V5, ProtocolVersion::V311] {
        assert!(
            delivered(
                Side::control(ProtocolVersion::V5),
                Side::data_streams(version),
                QoS::AtLeastOnce,
                LONG_PAYLOAD,
            )
            .await,
            "{version:?}"
        );
    }
}

#[tokio::test]
async fn publish_in_a_datagram_is_delivered() {
    for version in [ProtocolVersion::V5, ProtocolVersion::V311] {
        assert!(
            delivered(
                Side::datagrams(version),
                Side::control(ProtocolVersion::V5),
                QoS::AtMostOnce,
                SHORT_PAYLOAD,
            )
            .await,
            "{version:?}"
        );
    }
}

#[tokio::test]
async fn publish_on_the_control_stream_is_delivered() {
    for version in [ProtocolVersion::V5, ProtocolVersion::V311] {
        assert!(
            delivered(
                Side::control(version),
                Side::control(ProtocolVersion::V5),
                QoS::AtLeastOnce,
                SHORT_PAYLOAD,
            )
            .await,
            "{version:?}"
        );
    }
}

#[tokio::test]
async fn long_message_on_a_broker_opened_stream_is_delivered() {
    for version in [ProtocolVersion::V5, ProtocolVersion::V311] {
        assert!(
            delivered(
                Side::control(ProtocolVersion::V5),
                Side::control(version),
                QoS::AtLeastOnce,
                LONG_PAYLOAD,
            )
            .await,
            "{version:?}"
        );
    }
}

#[tokio::test]
async fn short_message_on_a_broker_opened_stream_is_delivered() {
    for version in [ProtocolVersion::V5, ProtocolVersion::V311] {
        assert!(
            delivered(
                Side::control(ProtocolVersion::V5),
                Side::control(version),
                QoS::AtLeastOnce,
                SHORT_PAYLOAD,
            )
            .await,
            "{version:?}"
        );
    }
}
