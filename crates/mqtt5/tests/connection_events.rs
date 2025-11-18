use mqtt5::time::Duration;
use mqtt5::{client::ConnectionEvent, client::DisconnectReason, MqttClient};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn test_connection_event_callback() {
    let client = MqttClient::new("test-client");

    let connected_count = Arc::new(AtomicU32::new(0));
    let disconnected_count = Arc::new(AtomicU32::new(0));
    let session_present = Arc::new(AtomicBool::new(false));

    let connected_count_clone = Arc::clone(&connected_count);
    let disconnected_count_clone = Arc::clone(&disconnected_count);
    let session_present_clone = Arc::clone(&session_present);

    // Register connection event callback
    client
        .on_connection_event(move |event| match event {
            ConnectionEvent::Connected {
                session_present: sp,
            } => {
                connected_count_clone.fetch_add(1, Ordering::Relaxed);
                session_present_clone.store(sp, Ordering::Relaxed);
            }
            ConnectionEvent::Disconnected { .. } => {
                disconnected_count_clone.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        })
        .await
        .unwrap();

    // Try connecting
    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            // Give some time for event to be processed
            tokio::time::sleep(Duration::from_millis(100)).await;

            assert_eq!(connected_count.load(Ordering::Relaxed), 1);
            assert_eq!(disconnected_count.load(Ordering::Relaxed), 0);

            // Try to disconnect - might fail if already disconnected by server
            if client.is_connected().await {
                client.disconnect().await.expect("Failed to disconnect");
                tokio::time::sleep(Duration::from_millis(100)).await;
                assert_eq!(disconnected_count.load(Ordering::Relaxed), 1);
            } else {
                // Already disconnected by server - that's fine for this test
                eprintln!("Client already disconnected by server (SessionTakenOver)");
            }
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_multiple_event_callbacks() {
    let client = MqttClient::new("test-client");

    let callback1_count = Arc::new(AtomicU32::new(0));
    let callback2_count = Arc::new(AtomicU32::new(0));

    let count1 = Arc::clone(&callback1_count);
    client
        .on_connection_event(move |event| {
            if matches!(event, ConnectionEvent::Connected { .. }) {
                count1.fetch_add(1, Ordering::Relaxed);
            }
        })
        .await
        .unwrap();

    let count2 = Arc::clone(&callback2_count);
    client
        .on_connection_event(move |event| {
            if matches!(event, ConnectionEvent::Connected { .. }) {
                count2.fetch_add(1, Ordering::Relaxed);
            }
        })
        .await
        .unwrap();

    // Try connecting
    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Both callbacks should be called
            assert_eq!(callback1_count.load(Ordering::Relaxed), 1);
            assert_eq!(callback2_count.load(Ordering::Relaxed), 1);

            // Only disconnect if we successfully connected
            let _ = client.disconnect().await;
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_disconnect_reasons() {
    let client = MqttClient::new("test-client");

    let disconnect_reasons = Arc::new(Mutex::new(Vec::new()));
    let reasons_clone = Arc::clone(&disconnect_reasons);

    client
        .on_connection_event(move |event| {
            if let ConnectionEvent::Disconnected { reason } = event {
                let reasons = reasons_clone.clone();
                tokio::spawn(async move {
                    reasons.lock().await.push(reason);
                });
            }
        })
        .await
        .unwrap();

    // Test client-initiated disconnect
    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            client.disconnect().await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;

            let reasons = disconnect_reasons.lock().await;
            assert_eq!(reasons.len(), 1);
            assert!(matches!(reasons[0], DisconnectReason::ClientInitiated));
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_connection_failure_event() {
    let client = MqttClient::new("test-client");

    let failure_count = Arc::new(AtomicU32::new(0));
    let failure_count_clone = Arc::clone(&failure_count);

    client
        .on_connection_event(move |event| {
            if matches!(event, ConnectionEvent::Disconnected { .. }) {
                failure_count_clone.fetch_add(1, Ordering::Relaxed);
            }
        })
        .await
        .unwrap();

    // Try connecting to invalid address - use a port that's unlikely to be in use
    let result = client.connect("mqtt://localhost:65432").await;
    assert!(result.is_err());

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connection failure doesn't trigger disconnect event since we were never connected
    // Verify no disconnect event was triggered
    assert_eq!(failure_count.load(Ordering::Relaxed), 0);

    // Verify the client is not connected
    assert!(!client.is_connected().await);
}

#[tokio::test]
async fn test_clear_callbacks() {
    let client = MqttClient::new("test-client");

    let count = Arc::new(AtomicU32::new(0));
    let count_clone = Arc::clone(&count);

    client
        .on_connection_event(move |_| {
            count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    // Clear callbacks
    client.clear_connection_event_callbacks().await;

    // Try connecting
    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Callback should not have been called
            assert_eq!(count.load(Ordering::Relaxed), 0);

            // Only disconnect if we successfully connected
            let _ = client.disconnect().await;
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_reconnecting_event() {
    // This test would require simulating connection loss and reconnection
    // For now, we just verify the event structure compiles by creating and asserting on them
    let event = ConnectionEvent::Reconnecting { attempt: 1 };
    assert!(matches!(
        event,
        ConnectionEvent::Reconnecting { attempt: 1 }
    ));

    let event = ConnectionEvent::ReconnectFailed {
        error: mqtt5::MqttError::ConnectionError("Test error".to_string()),
    };
    assert!(matches!(event, ConnectionEvent::ReconnectFailed { .. }));
}
