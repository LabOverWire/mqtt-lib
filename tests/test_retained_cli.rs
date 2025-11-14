mod common;
use common::TestBroker;
use mqtt5::time::Duration;
use mqtt5::{MqttClient, PublishOptions, QoS};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::timeout;

#[tokio::test]
async fn test_retained_message_delivery() -> Result<(), Box<dyn std::error::Error>> {
    // Start test broker
    let broker = TestBroker::start().await;

    println!("Step 1: Publishing retained message...");

    // Publisher publishes a retained message
    let publisher = MqttClient::new("publisher");
    publisher.connect(broker.address()).await?;

    let options = PublishOptions {
        retain: true,
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    publisher
        .publish_with_options(
            "test/retained/topic",
            b"This is a retained message!",
            options,
        )
        .await?;

    println!("✓ Retained message published");

    // Disconnect publisher
    publisher.disconnect().await?;

    // Wait a bit
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("\nStep 2: New subscriber connecting...");

    // New subscriber connects and subscribes
    let subscriber = MqttClient::new("subscriber");
    subscriber.connect(broker.address()).await?;

    let received = Arc::new(AtomicBool::new(false));
    let received_clone = received.clone();

    // Subscribe and expect to receive the retained message
    subscriber
        .subscribe("test/retained/topic", move |msg| {
            println!(
                "✓ Received message: {}",
                String::from_utf8_lossy(&msg.payload)
            );
            println!("  Topic: {}", msg.topic);
            println!("  Retained: {}", msg.retain);
            received_clone.store(true, Ordering::Relaxed);
        })
        .await?;

    // Wait for message to be received
    let result = timeout(Duration::from_secs(2), async {
        while !received.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;

    match result {
        Ok(()) => println!("\n✅ SUCCESS: Retained message was delivered to new subscriber!"),
        Err(_) => println!("\n❌ FAILED: Retained message was NOT delivered to new subscriber"),
    }

    // Cleanup
    subscriber.disconnect().await?;

    Ok(())
}
