// Debug test for mqtt-v5 TLS connection
mod common;
use common::TestBroker;

#[cfg(test)]
mod tests {
    use super::*;
    use mqtt5::time::Duration;
    use mqtt5::transport::tls::TlsConfig;
    use mqtt5::{ConnectOptions, MqttClient};

    #[tokio::test]
    async fn debug_tls_connection() {
        // Start test broker with TLS
        let broker = TestBroker::start_with_tls().await;

        // Extract address from broker (format: mqtts://host:port)
        let broker_addr = broker.address().strip_prefix("mqtts://").unwrap();
        let addr: std::net::SocketAddr = broker_addr.parse().unwrap();

        // Create client
        let client = MqttClient::new("debug-tls-client");

        println!("Connecting to address: {addr:?}");

        // Create TLS config
        let mut tls_config = TlsConfig::new(addr, "localhost");

        // For localhost testing with self-signed certificates, disable verification
        tls_config = tls_config.with_verify_server_cert(false);

        // Load CA certificate
        tls_config
            .load_ca_cert_pem("test_certs/ca.pem")
            .expect("Failed to load CA cert");

        println!("TLS config created, attempting connection...");

        // Create connection options
        let options = ConnectOptions::new("debug-tls-client")
            .with_clean_start(true)
            .with_keep_alive(Duration::from_secs(30));

        // Attempt connection
        match client
            .connect_with_tls_and_options(tls_config, options)
            .await
        {
            Ok(_) => {
                println!("✅ TLS connection successful!");

                // Test publish
                match client.publish("test/debug", b"Hello TLS").await {
                    Ok(_) => println!("✅ Publish successful!"),
                    Err(e) => println!("❌ Publish failed: {e}"),
                }

                // Disconnect
                let _ = client.disconnect().await;
            }
            Err(e) => {
                println!("❌ TLS connection failed: {e}");
                panic!("TLS connection failed: {e}");
            }
        }
    }
}
