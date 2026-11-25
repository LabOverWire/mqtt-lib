#![cfg(feature = "broker")]

use mqtt5::broker::storage::{
    FileBackend, InflightDirection, InflightMessage, InflightPhase, StorageBackend,
};
use mqtt5::packet::publish::PublishPacket;
use mqtt5::QoS;
use std::sync::Arc;

fn inflight(client_id: &str, packet_id: u16) -> InflightMessage {
    let publish = PublishPacket::new("t".to_string(), b"x".to_vec(), QoS::ExactlyOnce)
        .with_packet_id(packet_id);
    InflightMessage::from_publish(
        &publish,
        client_id.to_string(),
        InflightDirection::Outbound,
        InflightPhase::AwaitingPubrec,
    )
}

async fn lost_writes(with_cleanup: bool) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(FileBackend::new(dir.path()).await.unwrap());
    let clients: Vec<String> = (0..64).map(|index| format!("live-{index}")).collect();
    let mut lost = Vec::new();
    for round in 0..40u16 {
        let packet_id = round + 2;
        for client_id in &clients {
            backend
                .store_inflight_message(inflight(client_id, 1))
                .await
                .unwrap();
        }
        backend.flush_queue_writes().await;
        for client_id in &clients {
            backend
                .remove_inflight_message(client_id, 1, InflightDirection::Outbound)
                .await
                .unwrap();
        }
        backend.flush_queue_writes().await;
        let writer = Arc::clone(&backend);
        let ids = clients.clone();
        let new_records = tokio::spawn(async move {
            for client_id in &ids {
                writer
                    .store_inflight_message(inflight(client_id, packet_id))
                    .await
                    .unwrap();
                writer.flush_queue_writes().await;
            }
        });
        if with_cleanup {
            let cleaned = backend.cleanup_expired().await;
            assert!(cleaned.is_ok(), "{cleaned:?}");
        }
        new_records.await.unwrap();
        for client_id in &clients {
            let held = backend.get_inflight_messages(client_id).await.unwrap();
            if !held.iter().any(|m| m.packet_id == packet_id) {
                lost.push(format!("round {round} {client_id}"));
            }
            backend
                .remove_inflight_message(client_id, packet_id, InflightDirection::Outbound)
                .await
                .unwrap();
        }
        backend.flush_queue_writes().await;
    }
    lost
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn periodic_cleanup_never_loses_an_inflight_write_into_an_emptied_directory() {
    let lost = lost_writes(true).await;
    assert!(lost.is_empty(), "lost {} writes: {lost:?}", lost.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_without_cleanup_no_inflight_write_is_lost() {
    let lost = lost_writes(false).await;
    assert!(lost.is_empty(), "lost {} writes: {lost:?}", lost.len());
}
