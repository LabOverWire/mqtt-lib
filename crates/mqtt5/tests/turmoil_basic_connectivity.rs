//! Basic connectivity tests using Turmoil
//!
//! These tests verify that the broker can accept connections and handle
//! basic MQTT operations in a deterministic environment.

#[cfg(feature = "turmoil-testing")]
use mqtt5::testing::{TurmoilBroker, TurmoilBrokerConfig, TurmoilClient, TurmoilClientConfig};
#[cfg(feature = "turmoil-testing")]
use mqtt5::time::Duration;

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_basic_broker_startup() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(10))
        .build();

    // Add broker host
    sim.host("broker", || async {
        let config = TurmoilBrokerConfig::new("0.0.0.0:1883");
        let broker = TurmoilBroker::new(config)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

        broker
            .run()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    });

    // Run the simulation
    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_client_connection() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(15))
        .build();

    // Add broker host
    sim.host("broker", || async {
        let config = TurmoilBrokerConfig::new("0.0.0.0:1883");
        let broker = TurmoilBroker::new(config)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

        broker
            .run()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    });

    // Add client host
    sim.host("client", || async {
        // Wait a bit for broker to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        let config = TurmoilClientConfig::new("test-client");
        let client = TurmoilClient::new(&config);

        client
            .connect("broker:1883")
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

        // Verify connection
        assert!(client.is_connected().await);

        // Disconnect cleanly
        client
            .disconnect()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    // Run the simulation
    sim.run().unwrap();
}

#[cfg(feature = "turmoil-testing")]
#[test]
fn test_multiple_clients_connection() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(20))
        .build();

    // Add broker host
    sim.host("broker", || async {
        let config = TurmoilBrokerConfig::new("0.0.0.0:1883");
        let broker = TurmoilBroker::new(config)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

        broker
            .run()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
    });

    // Add multiple client hosts
    for i in 1..=3 {
        let client_name = format!("client{i}");
        sim.host(client_name, move || async move {
            tokio::time::sleep(Duration::from_millis(200)).await;

            let config = TurmoilClientConfig::new(&format!("test-client-{i}"));
            let client = TurmoilClient::new(&config);

            client
                .connect("broker:1883")
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

            assert!(client.is_connected().await);

            // Stay connected for a while
            tokio::time::sleep(Duration::from_secs(5)).await;

            client
                .disconnect()
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

            Ok::<(), Box<dyn std::error::Error>>(())
        });
    }

    // Run the simulation
    sim.run().unwrap();
}
