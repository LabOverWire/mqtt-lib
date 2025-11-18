mod common;
use common::TestBroker;
use mqtt5::time::Duration;
use mqtt5::MqttClient;

#[tokio::test]
async fn test_simple_connection_and_subscribe() {
    // Start test broker
    let broker = TestBroker::start().await;

    println!("Creating client...");
    let client = MqttClient::new("debug-client");

    println!("Connecting to {}...", broker.address());
    match client.connect(broker.address()).await {
        Ok(()) => println!("Connected successfully"),
        Err(e) => {
            eprintln!("Failed to connect: {e:?}");
            return;
        }
    }

    println!("Setting up subscription...");
    let result = client
        .subscribe("test/topic", |msg| {
            println!("Received message on {}: {:?}", msg.topic, msg.payload);
        })
        .await;

    match result {
        Ok(qos) => println!("Subscribed with QoS: {qos:?}"),
        Err(e) => {
            eprintln!("Failed to subscribe: {e:?}");
            return;
        }
    }

    println!("Publishing message...");
    match client.publish("test/topic", b"Hello MQTT").await {
        Ok(packet_id) => println!("Published with packet_id: {packet_id:?}"),
        Err(e) => {
            eprintln!("Failed to publish: {e:?}");
            return;
        }
    }

    println!("Waiting for message...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    println!("Disconnecting...");
    match client.disconnect().await {
        Ok(()) => println!("Disconnected successfully"),
        Err(e) => eprintln!("Failed to disconnect: {e:?}"),
    }
}
