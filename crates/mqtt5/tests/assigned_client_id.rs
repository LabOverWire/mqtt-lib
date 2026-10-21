#![cfg(feature = "broker")]

mod common;

use common::TestBroker;
use mqtt5::{ConnectOptions, MqttClient};

async fn unnamed_client_id(broker: &TestBroker) -> String {
    let options = ConnectOptions::new("")
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let client = MqttClient::with_options(options.clone());
    let session_present = Box::pin(client.connect_with_options(broker.address(), options))
        .await
        .map(|result| result.session_present);
    assert!(
        matches!(session_present, Ok(false)),
        "unnamed client was given an existing session: {session_present:?}"
    );
    let client_id = client.client_id().await;
    client
        .disconnect()
        .await
        .expect("unnamed client disconnects");
    client_id
}

#[tokio::test]
async fn an_unnamed_client_never_gets_an_existing_session() {
    let broker = TestBroker::start().await;
    let options = ConnectOptions::new("auto-0")
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let owner = MqttClient::with_options(options.clone());
    Box::pin(owner.connect_with_options(broker.address(), options))
        .await
        .expect("named client connects");
    owner
        .subscribe("assigned/owner", |_| {})
        .await
        .expect("named client subscribes");
    owner.disconnect().await.expect("named client disconnects");

    let first = unnamed_client_id(&broker).await;
    let second = unnamed_client_id(&broker).await;

    assert_ne!(first, second);
    for id in [&first, &second] {
        let suffix = id.strip_prefix("auto-").expect("assigned id prefix");
        assert_eq!(suffix.len(), 32, "{id}");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }
}
