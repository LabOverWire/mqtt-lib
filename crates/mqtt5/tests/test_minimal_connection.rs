mod common;
use common::TestBroker;
use mqtt5::MqttClient;

#[tokio::test]
async fn test_minimal_connection() {
    // Start test broker
    let broker = TestBroker::start().await;

    println!("Creating client...");
    let client = MqttClient::new("test-minimal-client");

    println!("Connecting to {}...", broker.address());
    match client.connect(broker.address()).await {
        Ok(()) => {
            println!("Connected successfully!");
            assert!(client.is_connected().await);

            println!("Disconnecting...");
            client.disconnect().await.unwrap();
            assert!(!client.is_connected().await);
            println!("Test passed!");
        }
        Err(e) => {
            println!("Connection failed: {e:?}");
            println!("Error type: {e}");
            panic!("Failed to connect");
        }
    }
}
