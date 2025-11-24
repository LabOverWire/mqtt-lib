use mqtt5::error::MqttError;
use mqtt5::packet::connack::ConnAckPacket;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::session::flow_control::TopicAliasManager;
use mqtt5::types::ReasonCode;
use mqtt5::QoS;

#[test]
fn test_topic_alias_manager_basic() {
    let mut tam = TopicAliasManager::new(5);

    // First alias
    let alias1 = tam.get_or_create_alias("topic/1").unwrap();
    assert_eq!(alias1, 1);
    assert_eq!(tam.get_topic(1), Some("topic/1"));
    assert_eq!(tam.get_alias("topic/1"), Some(1));

    // Second alias
    let alias2 = tam.get_or_create_alias("topic/2").unwrap();
    assert_eq!(alias2, 2);

    // Same topic returns same alias
    let alias1_again = tam.get_or_create_alias("topic/1").unwrap();
    assert_eq!(alias1_again, 1);
}

#[test]
fn test_topic_alias_manager_limit() {
    let mut tam = TopicAliasManager::new(2);

    // Fill up the limit
    let alias1 = tam.get_or_create_alias("topic/1").unwrap();
    let alias2 = tam.get_or_create_alias("topic/2").unwrap();

    assert_eq!(alias1, 1);
    assert_eq!(alias2, 2);

    // Should fail when limit is reached
    let alias3 = tam.get_or_create_alias("topic/3");
    assert!(alias3.is_none());
}

#[test]
fn test_topic_alias_manager_register() {
    let mut tam = TopicAliasManager::new(10);

    // Register alias from peer
    tam.register_alias(5, "remote/topic").unwrap();
    assert_eq!(tam.get_topic(5), Some("remote/topic"));
    assert_eq!(tam.get_alias("remote/topic"), Some(5));

    // Update existing alias
    tam.register_alias(5, "new/topic").unwrap();
    assert_eq!(tam.get_topic(5), Some("new/topic"));
    assert!(tam.get_alias("remote/topic").is_none());
}

#[test]
fn test_topic_alias_manager_invalid() {
    let mut tam = TopicAliasManager::new(10);

    // Zero alias is invalid
    let result = tam.register_alias(0, "topic");
    assert!(result.is_err());
    if let Err(MqttError::TopicAliasInvalid(alias)) = result {
        assert_eq!(alias, 0);
    }

    // Alias beyond maximum is invalid
    let result = tam.register_alias(11, "topic");
    assert!(result.is_err());
    if let Err(MqttError::TopicAliasInvalid(alias)) = result {
        assert_eq!(alias, 11);
    }
}

#[test]
fn test_topic_alias_manager_clear() {
    let mut tam = TopicAliasManager::new(10);

    let _ = tam.get_or_create_alias("topic/1");
    let _ = tam.get_or_create_alias("topic/2");
    tam.register_alias(5, "topic/5").unwrap();

    tam.clear();

    assert!(tam.get_topic(1).is_none());
    assert!(tam.get_topic(5).is_none());
    assert!(tam.get_alias("topic/1").is_none());
}

#[test]
fn test_topic_alias_manager_wraparound() {
    let mut tam = TopicAliasManager::new(3);

    // Fill up aliases
    let alias1 = tam.get_or_create_alias("topic/1").unwrap();
    let alias2 = tam.get_or_create_alias("topic/2").unwrap();
    let alias3 = tam.get_or_create_alias("topic/3").unwrap();

    assert_eq!(alias1, 1);
    assert_eq!(alias2, 2);
    assert_eq!(alias3, 3);

    // Clear one alias
    tam.clear();

    // Next allocation should start from 1 again
    let alias_new = tam.get_or_create_alias("topic/new").unwrap();
    assert_eq!(alias_new, 1);
}

#[test]
fn test_publish_packet_topic_alias() {
    let packet = PublishPacket::new("test/topic", b"payload", QoS::AtLeastOnce).with_topic_alias(5);

    assert_eq!(packet.topic_alias(), Some(5));
}

#[test]
fn test_publish_packet_no_topic_alias() {
    let packet = PublishPacket::new("test/topic", b"payload", QoS::AtLeastOnce);

    assert_eq!(packet.topic_alias(), None);
}

#[test]
fn test_connack_topic_alias_maximum() {
    let connack = ConnAckPacket::new(false, ReasonCode::Success).with_topic_alias_maximum(100);

    assert_eq!(connack.topic_alias_maximum(), Some(100));
}

#[test]
fn test_connack_no_topic_alias_maximum() {
    let connack = ConnAckPacket::new(false, ReasonCode::Success);

    assert_eq!(connack.topic_alias_maximum(), None);
}

#[tokio::test]
async fn test_topic_alias_integration() {
    use mqtt5::session::state::{SessionConfig, SessionState};

    let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

    // Set topic alias maximums
    session.set_topic_alias_maximum_out(10).await;
    session.set_topic_alias_maximum_in(10).await;

    // Test outgoing aliases
    let alias1 = session.get_or_create_topic_alias("topic/1").await;
    assert_eq!(alias1, Some(1));

    let alias1_again = session.get_or_create_topic_alias("topic/1").await;
    assert_eq!(alias1_again, Some(1));

    // Test incoming aliases
    session
        .register_incoming_topic_alias(5, "incoming/topic")
        .await
        .unwrap();
    let topic = session.get_topic_for_alias(5).await;
    assert_eq!(topic, Some("incoming/topic".to_string()));
}

#[tokio::test]
async fn test_topic_alias_disabled() {
    use mqtt5::session::state::{SessionConfig, SessionState};

    let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

    // Topic alias maximum is 0 by default (disabled)
    let alias = session.get_or_create_topic_alias("topic/1").await;
    assert_eq!(alias, None);
}

#[tokio::test]
async fn test_topic_alias_publish_flow() {
    use mqtt5::session::state::{SessionConfig, SessionState};

    let session = SessionState::new("test-client".to_string(), SessionConfig::default(), true);

    session.set_topic_alias_maximum_out(10).await;

    // First publish - should get alias but keep topic name
    let topic = "sensors/temperature";
    let alias1 = session.get_or_create_topic_alias(topic).await;
    assert_eq!(alias1, Some(1));

    // Check that alias is registered
    {
        let tam = session.topic_alias_out().read().await;
        assert_eq!(tam.get_alias(topic), Some(1));
    }

    // Second publish of same topic - alias already exists
    {
        let tam = session.topic_alias_out().read().await;
        assert_eq!(tam.get_alias(topic), Some(1));
    }
}

#[test]
fn test_topic_alias_edge_cases() {
    let mut tam = TopicAliasManager::new(u16::MAX);

    // Can use maximum alias value
    tam.register_alias(u16::MAX, "topic/max").unwrap();
    assert_eq!(tam.get_topic(u16::MAX), Some("topic/max"));

    // Empty topic name is valid
    tam.register_alias(1, "").unwrap();
    assert_eq!(tam.get_topic(1), Some(""));

    // Very long topic name
    let long_topic = "a".repeat(1000);
    tam.register_alias(2, &long_topic).unwrap();
    assert_eq!(tam.get_topic(2), Some(long_topic.as_str()));
}

#[test]
fn test_topic_alias_replacement() {
    let mut tam = TopicAliasManager::new(10);

    // Register initial mapping
    tam.register_alias(1, "topic/old").unwrap();
    assert_eq!(tam.get_topic(1), Some("topic/old"));
    assert_eq!(tam.get_alias("topic/old"), Some(1));

    // Replace with new topic
    tam.register_alias(1, "topic/new").unwrap();
    assert_eq!(tam.get_topic(1), Some("topic/new"));
    assert_eq!(tam.get_alias("topic/new"), Some(1));

    // Old topic should no longer have an alias
    assert!(tam.get_alias("topic/old").is_none());
}
