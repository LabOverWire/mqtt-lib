#![cfg(feature = "broker")]
#![cfg(feature = "transport-quic")]

use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::MqttClient;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const HEADER: &str = "timestamp_ns,rtt_us,cwnd,lost_packets,congestion_events,sent_packets,stream_data_blocked,data_blocked,streams_blocked_uni";

fn stats_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn broker_writes_quic_stats_per_connection_when_the_directory_is_set() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let stats_dir = tempfile::tempdir().unwrap();
    std::env::set_var("MQTT5_QUIC_STATS_DIR", stats_dir.path());

    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            BrokerQuicConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .with_stream_receive_window(2048),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker
        .quic_local_addr()
        .expect("QUIC endpoint must be bound");
    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    let client = MqttClient::new("quic-stats-sampler");
    client.set_insecure_tls(true).await;
    client.connect(&format!("quic://{addr}")).await.unwrap();
    let payload = vec![0u8; 4096];
    for _ in 0..50 {
        client
            .publish("quic-stats/burst", payload.clone())
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    client.disconnect().await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let rows = loop {
        let files = stats_files(stats_dir.path());
        assert!(files.len() <= 1, "one file per connection: {files:?}");
        let rows: Vec<String> = files
            .first()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default();
        let final_row_written = rows
            .last()
            .is_some_and(|row| row.split(',').nth(6).is_some_and(|v| v != "0"));
        if final_row_written || tokio::time::Instant::now() >= deadline {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    std::env::remove_var("MQTT5_QUIC_STATS_DIR");
    broker_handle.abort();

    assert_eq!(rows.first().map(String::as_str), Some(HEADER));
    assert!(rows.len() >= 3, "expected samples, got {rows:?}");
    let columns = HEADER.split(',').count();
    assert!(rows.iter().all(|row| row.split(',').count() == columns));
    let last: Vec<u64> = rows
        .last()
        .unwrap()
        .split(',')
        .map(|v| v.parse().unwrap())
        .collect();
    assert!(last[5] > 0, "sent_packets");
    assert!(
        last[6] > 0,
        "the client's STREAM_DATA_BLOCKED must be counted: {last:?}"
    );
}
