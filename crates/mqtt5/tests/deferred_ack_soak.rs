#![cfg(feature = "broker")]
#![allow(clippy::large_futures)]

//! Sustained backpressure + liveness soak for deferred acknowledgement.
//!
//! Fills the Receive-Maximum window with held `AckToken`s under a burst load and asserts the four
//! things the feature promises under saturation: the broker throttles to exactly `receive_maximum`
//! outstanding (obligation 3), the resident token count stays bounded (no unbounded buffering,
//! §3.3), the control plane is never blocked while saturated (obligation 4), and once the tokens
//! drain every message is delivered exactly once with no loss.

mod common;

use common::TestBroker;
use mqtt5::time::Duration;
use mqtt5::{AckToken, ConnectOptions, MqttClient, PublishOptions, QoS, SubscribeOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use ulid::Ulid;

const RECEIVE_MAXIMUM: u16 = 4;
const BURST: u32 = 40;

fn client_id(name: &str) -> String {
    format!("test-{name}-{}", Ulid::new())
}

fn deferred_options(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_receive_maximum(RECEIVE_MAXIMUM)
        .with_keep_alive(Duration::from_secs(5))
}

fn qos2_sub() -> SubscribeOptions {
    SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    }
}

fn qos2_pub() -> PublishOptions {
    PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    }
}

async fn wait_until(cond: impl Fn() -> bool) -> bool {
    for _ in 0..400 {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

#[tokio::test]
async fn deferred_ack_backpressure_soak_holds_bounded_then_drains_without_loss() {
    let broker = TestBroker::start().await;
    let subscriber = MqttClient::with_options(deferred_options(&client_id("soak-sub")));
    subscriber.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let held: Arc<Mutex<Vec<AckToken>>> = Arc::new(Mutex::new(Vec::new()));
    let peak_held = Arc::new(AtomicU32::new(0));

    let rc = Arc::clone(&received);
    let hc = Arc::clone(&held);
    let pk = Arc::clone(&peak_held);
    subscriber
        .subscribe_with_ack("soak/#", qos2_sub(), move |_p, token| {
            rc.fetch_add(1, Ordering::SeqCst);
            let mut guard = hc.lock().unwrap();
            guard.push(token);
            let len = u32::try_from(guard.len()).unwrap_or(u32::MAX);
            pk.fetch_max(len, Ordering::SeqCst);
        })
        .await
        .unwrap();

    // Burst well beyond the window; every delivered token is held (never acked yet).
    let publisher = MqttClient::new(client_id("soak-pub"));
    publisher.connect(broker.address()).await.unwrap();
    for i in 0..BURST {
        publisher
            .publish_with_options("soak/x", format!("m{i}").into_bytes(), qos2_pub())
            .await
            .unwrap();
    }

    // The window fills to exactly receive_maximum and stops.
    assert!(
        wait_until(|| received.load(Ordering::SeqCst) == u32::from(RECEIVE_MAXIMUM)).await,
        "broker fills the window to receive_maximum held tokens"
    );

    // Sustained hold: it must stay pinned at the window and never grow (bounded memory).
    for _ in 0..12 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            received.load(Ordering::SeqCst),
            u32::from(RECEIVE_MAXIMUM),
            "no delivery beyond the window while every token is held"
        );
        assert!(
            held.lock().unwrap().len() <= RECEIVE_MAXIMUM as usize,
            "resident held-token count never exceeds the window (no unbounded buffering)"
        );
    }

    // Control plane is not blocked while saturated: a fresh client can still connect and subscribe,
    // and the saturated subscriber stays connected.
    let probe = MqttClient::with_options(deferred_options(&client_id("soak-probe")));
    probe.connect(broker.address()).await.unwrap();
    probe
        .subscribe_with_ack("other/#", qos2_sub(), |_p, _t| {})
        .await
        .expect("a SUBSCRIBE must still succeed while another client's window is saturated");
    assert!(
        subscriber.is_connected().await,
        "the saturated subscriber stays connected (reader not blocked)"
    );

    // Drain: keep acking held tokens; each freed slot lets the next message flow, 1-for-1.
    let drainer_received = Arc::clone(&received);
    let drainer_held = Arc::clone(&held);
    let drain = tokio::spawn(async move {
        while drainer_received.load(Ordering::SeqCst) < BURST {
            let token = drainer_held.lock().unwrap().pop();
            if let Some(token) = token {
                token.ack();
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        // Drain any tokens delivered after the final increment.
        while let Some(token) = drainer_held.lock().unwrap().pop() {
            token.ack();
        }
    });

    assert!(
        wait_until(|| received.load(Ordering::SeqCst) == BURST).await,
        "every message is eventually delivered once the tokens drain (no loss)"
    );
    drain.await.unwrap();

    // No duplicate storm after the full drain.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        received.load(Ordering::SeqCst),
        BURST,
        "exactly the burst count is delivered — no loss, no duplicates"
    );
    assert_eq!(
        peak_held.load(Ordering::SeqCst),
        u32::from(RECEIVE_MAXIMUM),
        "the peak resident held-token count equals the window — backpressure stayed bounded throughout"
    );

    probe.disconnect().await.ok();
    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
}
