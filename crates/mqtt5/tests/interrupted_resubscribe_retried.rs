use mqtt5::{ConnectOptions, MqttClient, SubscribeOptions};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy)]
enum Script {
    SubAck,
    DropOnSubscribe,
}

async fn read_packet(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut first = [0u8; 1];
    stream.read_exact(&mut first).await.ok()?;
    let mut length = 0usize;
    let mut shift = 0;
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.ok()?;
        length |= usize::from(byte[0] & 0x7f) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await.ok()?;
    Some((first[0] >> 4, body))
}

async fn serve(
    mut stream: TcpStream,
    session_present: bool,
    script: Script,
    events: Arc<Mutex<Vec<usize>>>,
    index: usize,
) {
    let Some((1, _)) = read_packet(&mut stream).await else {
        return;
    };
    events.lock().unwrap().push(100 + index);
    let connack = [0x20, 0x03, u8::from(session_present), 0x00, 0x00];
    if stream.write_all(&connack).await.is_err() {
        return;
    }
    while let Some((kind, body)) = read_packet(&mut stream).await {
        match kind {
            8 => {
                events.lock().unwrap().push(index);
                match script {
                    Script::SubAck => {
                        let suback = [0x90, 0x04, body[0], body[1], 0x00, 0x01];
                        if stream.write_all(&suback).await.is_err() {
                            return;
                        }
                    }
                    Script::DropOnSubscribe => return,
                }
            }
            12 => {
                if stream.write_all(&[0xd0, 0x00]).await.is_err() {
                    return;
                }
            }
            14 => return,
            _ => {}
        }
    }
}

async fn scripted_server(plan: Vec<(bool, Script)>) -> (String, Arc<Mutex<Vec<usize>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    tokio::spawn(async move {
        for (index, (session_present, script)) in plan.into_iter().enumerate() {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(serve(
                stream,
                session_present,
                script,
                Arc::clone(&seen),
                index,
            ));
        }
    });
    (url, events)
}

fn persistent(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200))
}

#[derive(Clone, Copy)]
enum Subscription {
    Plain,
    DeferredAck,
}

fn options(id: &str, subscription: Subscription) -> ConnectOptions {
    match subscription {
        Subscription::Plain => persistent(id),
        Subscription::DeferredAck => persistent(id)
            .with_deferred_ack(true)
            .with_receive_maximum(16),
    }
}

async fn events_after_interrupted_restore(
    first_id: &str,
    second_id: &str,
    second_present: bool,
    subscription: Subscription,
) -> Vec<usize> {
    let (url, events) = scripted_server(vec![
        (false, Script::SubAck),
        (second_present, Script::DropOnSubscribe),
        (true, Script::SubAck),
    ])
    .await;
    let client = MqttClient::new(first_id);
    client
        .connect_with_options(&url, options(first_id, subscription))
        .await
        .unwrap();
    match subscription {
        Subscription::Plain => {
            client.subscribe("t/a", |_| {}).await.unwrap();
        }
        Subscription::DeferredAck => {
            client
                .subscribe_with_ack("t/a", SubscribeOptions::default(), |_, _| {})
                .await
                .unwrap();
        }
    }
    client.disconnect().await.unwrap();
    let reconnected = client
        .connect_with_options(&url, options(second_id, subscription))
        .await;
    tracing::info!(?reconnected, "second connect");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !events.lock().unwrap().contains(&2) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = client.disconnect().await;
    let recorded = events.lock().unwrap().clone();
    recorded
}

#[tokio::test]
async fn a_lost_session_resubscribe_cut_off_is_retried_when_the_session_resumes() {
    let recorded = events_after_interrupted_restore(
        "interrupted-a",
        "interrupted-a",
        false,
        Subscription::Plain,
    )
    .await;
    assert_eq!(
        recorded,
        vec![100, 0, 101, 1, 102, 2],
        "CONNECT (100 + n) and SUBSCRIBE (n) per connection index"
    );
}

#[tokio::test]
async fn a_new_id_resubscribe_cut_off_is_retried_when_the_session_resumes() {
    let recorded = events_after_interrupted_restore(
        "interrupted-b",
        "interrupted-c",
        true,
        Subscription::Plain,
    )
    .await;
    assert_eq!(
        recorded,
        vec![100, 0, 101, 1, 102, 2],
        "CONNECT (100 + n) and SUBSCRIBE (n) per connection index"
    );
}

#[tokio::test]
async fn a_lost_session_deferred_ack_resubscribe_cut_off_is_retried_when_the_session_resumes() {
    let recorded = events_after_interrupted_restore(
        "interrupted-ack",
        "interrupted-ack",
        false,
        Subscription::DeferredAck,
    )
    .await;
    assert_eq!(
        recorded,
        vec![100, 0, 101, 1, 102, 2],
        "CONNECT (100 + n) and SUBSCRIBE (n) per connection index"
    );
}

#[tokio::test]
async fn control_a_settled_restore_is_not_repeated_when_the_session_resumes() {
    let (url, events) = scripted_server(vec![
        (false, Script::SubAck),
        (false, Script::SubAck),
        (true, Script::SubAck),
    ])
    .await;
    let client = MqttClient::new("settled-restore");
    for _ in 0..3 {
        client
            .connect_with_options(&url, persistent("settled-restore"))
            .await
            .unwrap();
        if events.lock().unwrap().len() == 1 {
            client.subscribe("t/a", |_| {}).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        client.disconnect().await.unwrap();
    }
    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![100, 0, 101, 1, 102],
        "CONNECT (100 + n) and SUBSCRIBE (n) per connection index"
    );
}
