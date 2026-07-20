//! Deferred acknowledgement.
//!
//! A normal subscription acknowledges each message the moment the client hands it to the
//! application, so the acknowledgement says only "received". A deferred subscription instead
//! delivers each message with a move-only [`mqtt5::AckToken`] and withholds the
//! acknowledgement — the PUBACK for `QoS` 1, the PUBREC for `QoS` 2 — until the application
//! calls `token.ack()`. The acknowledgement then means "processed", and the inbound Receive
//! Maximum window becomes real end-to-end backpressure: the broker stops sending once the
//! application's unacknowledged messages fill the window. `token.reject(reason)` sends an
//! error acknowledgement, and dropping the token without resolving it auto-acknowledges with
//! a reason code so a forgotten token can never wedge the flow.
//!
//! Deferred ack requires a persistent session (the broker must retain it across a reconnect),
//! which is why the connection sets `clean_start(false)`, a non-zero session expiry, and a
//! non-zero Receive Maximum. Delivery is at-least-once, so the callback must be idempotent.
//!
//! Start a broker on 127.0.0.1:1883 (or set `MQTT_BROKER`), then:
//!
//! ```bash
//! cargo run --example deferred_ack
//! ```

use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::{ConnectOptions, MqttClient, QoS, SubscribeOptions};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let broker =
        std::env::var("MQTT_BROKER").unwrap_or_else(|_| "mqtt://127.0.0.1:1883".to_string());

    let options = ConnectOptions::new("deferred-ack-demo")
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_receive_maximum(16);

    let client = MqttClient::with_options(options);
    client.connect(&broker).await?;

    let subscribe_options = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };

    client
        .subscribe_with_ack("jobs/#", subscribe_options, |publish, token| match process(
            &publish.topic_name,
            &publish.payload,
        ) {
            Ok(()) => token.ack(),
            Err(error) => {
                eprintln!("rejecting {}: {error}", publish.topic_name);
                token.reject(ReasonCode::UnspecifiedError);
            }
        })
        .await?;

    println!("Waiting for jobs on jobs/# (Ctrl-C to exit)...");
    tokio::time::sleep(Duration::from_secs(60)).await;

    client.disconnect().await?;
    Ok(())
}

fn process(topic: &str, payload: &[u8]) -> Result<(), String> {
    if payload.is_empty() {
        return Err(format!("empty payload on {topic}"));
    }
    println!("processing {topic} ({} bytes)", payload.len());
    Ok(())
}
