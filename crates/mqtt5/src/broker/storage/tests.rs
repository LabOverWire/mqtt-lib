//! Tests for the storage system

#[cfg(test)]
use super::super::*;
use crate::broker::storage::{ClientSession, QueuedMessage, RetainedMessage, StoredSubscription};
use crate::packet::publish::PublishPacket;
use crate::protocol::v5::properties::{PropertyId, PropertyValue};
use crate::time::Duration;
use crate::QoS;
use std::sync::Arc;
use tokio::time::sleep;

fn create_memory_storage() -> Storage<MemoryBackend> {
    Storage::new(MemoryBackend::new())
}

async fn create_file_storage() -> Storage<FileBackend> {
    let dir = tempfile::tempdir().unwrap();
    let backend = FileBackend::new(dir.path()).await.unwrap();
    Storage::new(backend)
}

#[tokio::test]
async fn test_retained_message_storage() {
    let storage = create_memory_storage();

    // Store retained message
    let packet = PublishPacket::new("test/topic", b"hello world", QoS::AtLeastOnce);
    let retained = RetainedMessage::new(packet);

    storage
        .store_retained("test/topic", retained.clone())
        .await
        .unwrap();

    // Retrieve it
    let retrieved = storage.get_retained("test/topic").await;
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().payload, b"hello world");

    // Remove it
    storage.remove_retained("test/topic").await.unwrap();
    assert!(storage.get_retained("test/topic").await.is_none());
}

#[tokio::test]
async fn test_retained_message_matching() {
    let storage = create_memory_storage();

    // Store multiple retained messages
    let topics = vec![
        "sensors/temp/room1",
        "sensors/temp/room2",
        "sensors/humidity/room1",
        "devices/light/room1",
    ];

    for topic in &topics {
        let packet = PublishPacket::new(*topic, topic.as_bytes(), QoS::AtMostOnce);
        let retained = RetainedMessage::new(packet);
        storage.store_retained(topic, retained).await.unwrap();
    }

    // Test wildcard matching
    let matches = storage.get_retained_matching("sensors/+/room1").await;
    assert_eq!(matches.len(), 2);

    let matches = storage.get_retained_matching("sensors/temp/+").await;
    assert_eq!(matches.len(), 2);

    let matches = storage.get_retained_matching("sensors/#").await;
    assert_eq!(matches.len(), 3);

    let matches = storage.get_retained_matching("devices/+/room1").await;
    assert_eq!(matches.len(), 1);
}

#[tokio::test]
async fn test_session_persistence() {
    let storage = create_memory_storage();

    // Create and store session with full subscription options
    let mut session = ClientSession::new("client1".to_string(), true, Some(3600));
    session.add_subscription(
        "test/+".to_string(),
        StoredSubscription {
            qos: QoS::AtLeastOnce,
            no_local: true,
            retain_as_published: true,
            retain_handling: 1,
            subscription_id: Some(42),
        },
    );
    session.add_subscription(
        "sensors/#".to_string(),
        StoredSubscription::new(QoS::ExactlyOnce),
    );

    storage.store_session(session.clone()).await.unwrap();

    // Retrieve session and verify full options
    let retrieved = storage.get_session("client1").await;
    assert!(retrieved.is_some());
    let retrieved_session = retrieved.unwrap();
    assert_eq!(retrieved_session.subscriptions.len(), 2);

    let sub = retrieved_session.subscriptions.get("test/+").unwrap();
    assert_eq!(sub.qos, QoS::AtLeastOnce);
    assert!(sub.no_local);
    assert!(sub.retain_as_published);
    assert_eq!(sub.retain_handling, 1);
    assert_eq!(sub.subscription_id, Some(42));

    let sub2 = retrieved_session.subscriptions.get("sensors/#").unwrap();
    assert_eq!(sub2.qos, QoS::ExactlyOnce);
    assert!(!sub2.no_local);
    assert!(!sub2.retain_as_published);

    // Remove subscription
    session.remove_subscription("test/+");
    storage.store_session(session.clone()).await.unwrap();

    let retrieved = storage.get_session("client1").await.unwrap();
    assert_eq!(retrieved.subscriptions.len(), 1);

    // Remove session
    storage.remove_session("client1").await.unwrap();
    assert!(storage.get_session("client1").await.is_none());
}

#[tokio::test]
async fn test_message_queuing() {
    let storage = create_memory_storage();

    // Queue messages
    let packet1 = PublishPacket::new("test/1", b"msg1", QoS::AtLeastOnce);
    let packet2 = PublishPacket::new("test/2", b"msg2", QoS::ExactlyOnce);

    let msg1 = QueuedMessage::new(packet1, "client1".to_string(), QoS::AtLeastOnce, Some(1));
    let msg2 = QueuedMessage::new(packet2, "client1".to_string(), QoS::ExactlyOnce, Some(2));

    storage.queue_message(msg1).await.unwrap();
    storage.queue_message(msg2).await.unwrap();

    // Retrieve queued messages
    let messages = storage.get_queued_messages("client1").await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].payload, b"msg1");
    assert_eq!(messages[1].payload, b"msg2");

    // Remove queued messages
    storage.remove_queued_messages("client1").await.unwrap();
    let messages = storage.get_queued_messages("client1").await.unwrap();
    assert_eq!(messages.len(), 0);
}

#[tokio::test]
async fn test_expiry_cleanup() {
    let storage = create_memory_storage();

    // Create message with very short expiry
    let mut packet = PublishPacket::new("test/expire", b"will expire", QoS::AtMostOnce);
    packet
        .properties
        .add(
            PropertyId::MessageExpiryInterval,
            PropertyValue::FourByteInteger(1),
        )
        .unwrap();

    let retained = RetainedMessage::new(packet);
    storage
        .store_retained("test/expire", retained)
        .await
        .unwrap();

    // Should exist immediately
    assert!(storage.get_retained("test/expire").await.is_some());

    // Wait for expiry
    sleep(Duration::from_secs(2)).await;

    // Cleanup should remove it
    storage.cleanup_expired().await.unwrap();
    assert!(storage.get_retained("test/expire").await.is_none());
}

#[tokio::test]
async fn test_file_backend_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let backend_path = dir.path().to_path_buf();

    // Create storage and store data
    {
        let backend = FileBackend::new(&backend_path).await.unwrap();
        let storage = Storage::new(backend);

        // Store retained message
        let packet = PublishPacket::new("persistent/topic", b"persistent data", QoS::AtLeastOnce);
        let retained = RetainedMessage::new(packet);
        storage
            .store_retained("persistent/topic", retained)
            .await
            .unwrap();

        // Store session
        let mut session = ClientSession::new("persistent_client".to_string(), true, None);
        session.add_subscription(
            "persistent/+".to_string(),
            StoredSubscription::new(QoS::AtLeastOnce),
        );
        storage.store_session(session).await.unwrap();
    }

    // Create new storage instance and verify data persisted
    {
        let backend = FileBackend::new(&backend_path).await.unwrap();
        let storage = Storage::new(backend);

        // Load retained messages
        storage.initialize().await.unwrap();

        // Check retained message
        let retained = storage.get_retained("persistent/topic").await;
        assert!(retained.is_some());
        assert_eq!(retained.unwrap().payload, b"persistent data");

        // Check session
        let session = storage
            .backend
            .get_session("persistent_client")
            .await
            .unwrap();
        assert!(session.is_some());
        assert_eq!(session.unwrap().subscriptions.len(), 1);
    }
}

#[tokio::test]
async fn test_concurrent_access() {
    let storage = Arc::new(create_memory_storage());

    // Spawn multiple tasks that access storage concurrently
    let mut handles = vec![];

    for i in 0..10 {
        let storage_clone = Arc::clone(&storage);
        let handle = tokio::spawn(async move {
            let topic = format!("concurrent/topic{i}");
            let packet = PublishPacket::new(&topic, format!("data{i}").as_bytes(), QoS::AtMostOnce);
            let retained = RetainedMessage::new(packet);
            storage_clone
                .store_retained(&topic, retained)
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all messages were stored
    let matches = storage.get_retained_matching("concurrent/+").await;
    assert_eq!(matches.len(), 10);
}

#[tokio::test]
async fn test_session_expiry() {
    let storage = create_memory_storage();

    // Create session with 1 second expiry
    let session = ClientSession::new("expiring_client".to_string(), true, Some(1));
    storage.store_session(session).await.unwrap();

    // Should exist immediately
    assert!(storage.get_session("expiring_client").await.is_some());

    // Wait for expiry
    sleep(Duration::from_secs(2)).await;

    // Should be expired
    assert!(storage.get_session("expiring_client").await.is_none());
}

#[tokio::test]
async fn test_dynamic_storage_backends() {
    // Test with memory backend
    let memory_backend = MemoryBackend::new();
    let dynamic = DynamicStorage::Memory(memory_backend);

    let packet = PublishPacket::new("dynamic/test", b"test data", QoS::AtMostOnce);
    let retained = RetainedMessage::new(packet);

    dynamic
        .store_retained_message("dynamic/test", retained.clone())
        .await
        .unwrap();
    let retrieved = dynamic.get_retained_message("dynamic/test").await.unwrap();
    assert!(retrieved.is_some());

    // Test with file backend
    let dir = tempfile::tempdir().unwrap();
    let file_backend = FileBackend::new(dir.path()).await.unwrap();
    let dynamic = DynamicStorage::File(file_backend);

    dynamic
        .store_retained_message("dynamic/test", retained)
        .await
        .unwrap();
    let retrieved = dynamic.get_retained_message("dynamic/test").await.unwrap();
    assert!(retrieved.is_some());
}
