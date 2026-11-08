#![cfg(feature = "broker")]

use bytes::BytesMut;
use mqtt5::broker::config::BrokerConfig;
use mqtt5::broker::router::{MessageRouter, SubscriptionRequest};
use mqtt5::broker::server::MqttBroker;
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use mqtt5_protocol::types::ConnectOptions;
use mqtt5_protocol::QoS;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::{sleep, Instant};

static LOG: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

struct Capture;

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LOG.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn withheld() -> usize {
    String::from_utf8_lossy(&LOG.lock().unwrap())
        .matches("Withholding PUBLISH routed for a subscription")
        .count()
}

fn subscribe_bytes(topics: &[&str]) -> Vec<u8> {
    let mut body = vec![0x00, 0x01, 0x00];
    for topic in topics {
        body.extend_from_slice(&u16::try_from(topic.len()).unwrap().to_be_bytes());
        body.extend_from_slice(topic.as_bytes());
        body.push(1);
    }
    let mut bytes = vec![0x82, u8::try_from(body.len()).unwrap()];
    bytes.extend_from_slice(&body);
    bytes
}

async fn broker_with_many_subscriptions() -> (String, Arc<MessageRouter>) {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_max_subscriptions_per_client(1_000_000);
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap().to_string();
    let router = broker.router();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    sleep(Duration::from_millis(100)).await;
    for i in 0..30_000 {
        router
            .subscribe(SubscriptionRequest::new(
                format!("other-{i}"),
                format!("other/{i}"),
                QoS::AtMostOnce,
            ))
            .await
            .unwrap();
    }
    (addr, router)
}

async fn subscribe_while_publishing(addr: &str, router: &Arc<MessageRouter>, attempt: u32) {
    let topic = format!("t/{attempt}");
    let topic = topic.as_str();
    let publishing = Arc::new(AtomicBool::new(true));
    let publisher = {
        let (router, publishing, topic) = (
            Arc::clone(router),
            Arc::clone(&publishing),
            topic.to_string(),
        );
        tokio::spawn(async move {
            while publishing.load(Ordering::SeqCst) {
                let publish = PublishPacket::new(topic.clone(), &b"m"[..], QoS::AtLeastOnce);
                router.route_message(&publish, Some("publisher")).await;
                tokio::task::yield_now().await;
            }
        })
    };
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut buffer = BytesMut::new();
    let mut connect = Vec::new();
    ConnectPacket::new(ConnectOptions::new(format!("sub-{attempt}")))
        .encode(&mut connect)
        .unwrap();
    stream.write_all(&connect).await.unwrap();
    assert!(matches!(
        read_packet_from_stream(&mut stream, 5, &mut buffer, 1 << 20).await,
        Ok(Packet::ConnAck(_))
    ));
    stream
        .write_all(&subscribe_bytes(&[topic, topic]))
        .await
        .unwrap();
    let until = Instant::now() + Duration::from_millis(400);
    while let Ok(Ok(packet)) = tokio::time::timeout_at(
        until,
        read_packet_from_stream(&mut stream, 5, &mut buffer, 1 << 20),
    )
    .await
    {
        if let Packet::Publish(publish) = packet {
            let id = publish.packet_id.unwrap().to_be_bytes();
            stream.write_all(&[0x40, 0x02, id[0], id[1]]).await.unwrap();
        }
    }
    publishing.store(false, Ordering::SeqCst);
    publisher.await.unwrap();
    let _ = stream.write_all(&[0xE0, 0x00]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subscribe_naming_a_new_filter_twice_withholds_nothing() {
    tracing_subscriber::fmt()
        .with_env_filter("mqtt5::broker::client_handler=debug")
        .with_writer(|| Capture)
        .with_ansi(false)
        .init();
    let (addr, router) = broker_with_many_subscriptions().await;
    for attempt in 0..40 {
        subscribe_while_publishing(&addr, &router, attempt).await;
    }
    assert_eq!(withheld(), 0);
}
