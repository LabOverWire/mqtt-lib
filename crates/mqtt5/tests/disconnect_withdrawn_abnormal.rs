use mqtt5::{ConnectOptions, MqttClient};
use std::future::Future;
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

fn sent_disconnect_after_connect(bytes: &[u8]) -> bool {
    if bytes.len() < 2 {
        return false;
    }
    let mut remaining = 0usize;
    let mut shift = 0;
    let mut index = 1;
    while index < bytes.len() {
        let byte = bytes[index];
        remaining |= usize::from(byte & 0x7f) << shift;
        index += 1;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let after = index + remaining;
    bytes.get(after).is_some_and(|&kind| kind == 0xE0)
}

async fn silent_peer(listener: &TcpListener) -> oneshot::Receiver<bool> {
    let (stream, _) = listener.accept().await.unwrap();
    let (report, outcome) = oneshot::channel();
    tokio::spawn(async move {
        let mut stream = stream;
        let mut received = Vec::new();
        let mut buffer = [0u8; 1024];
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer)).await {
                Ok(Ok(read)) if read > 0 => received.extend_from_slice(&buffer[..read]),
                _ => break,
            }
        }
        let _ = report.send(sent_disconnect_after_connect(&received));
    });
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn withdrawn_abnormal_disconnect_never_sends_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut violations = 0usize;
    let mut cancelled_by_withdrawn = 0usize;
    let mut finished_early = 0usize;
    let iterations = 3000u32;
    for iteration in 0..iterations {
        let client = MqttClient::with_options(
            ConnectOptions::new(format!("withdrawn-{iteration}")).with_automatic_reconnect(false),
        );
        let connecting = client.clone();
        let url = format!("mqtt://{addr}");
        let connect = tokio::spawn(async move { connecting.connect(&url).await });
        let outcome = silent_peer(&listener).await;
        tokio::time::sleep(Duration::from_millis(2)).await;

        let spin = Duration::from_nanos(u64::from(iteration % 300) * 100);
        let mut abnormal = Box::pin(client.disconnect_abnormally());
        let finished_at_once =
            std::future::poll_fn(|cx| Poll::Ready(abnormal.as_mut().poll(cx).is_ready())).await;
        if finished_at_once {
            finished_early += 1;
            tracing::warn!(
                iteration,
                connect_done = connect.is_finished(),
                "abnormal disconnect finished at once"
            );
        }
        let spin_start = Instant::now();
        while spin_start.elapsed() < spin {
            std::hint::spin_loop();
        }
        drop(abnormal);

        tokio::time::sleep(Duration::from_millis(2)).await;
        if connect.is_finished() {
            cancelled_by_withdrawn += 1;
        } else {
            let _ = client.disconnect_abnormally().await;
        }
        let _ = connect.await.unwrap();
        drop(client);
        if outcome.await.unwrap_or(false) {
            violations += 1;
            tracing::warn!(
                iteration,
                ?spin,
                "DISCONNECT sent with only an abnormal request"
            );
        }
    }
    assert_eq!(
        violations, 0,
        "{violations} of {iterations} connects sent DISCONNECT although only an abnormal disconnect was ever requested ({cancelled_by_withdrawn} were cancelled by the withdrawn request, {finished_early} abnormal disconnects finished at once)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn awaited_abnormal_disconnect_never_sends_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut violations = 0usize;
    let iterations = 1000u32;
    for iteration in 0..iterations {
        let client = MqttClient::with_options(
            ConnectOptions::new(format!("awaited-{iteration}")).with_automatic_reconnect(false),
        );
        let connecting = client.clone();
        let url = format!("mqtt://{addr}");
        let connect = tokio::spawn(async move { connecting.connect(&url).await });
        let outcome = silent_peer(&listener).await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        let _ = client.disconnect_abnormally().await;
        let _ = connect.await.unwrap();
        drop(client);
        if outcome.await.unwrap_or(false) {
            violations += 1;
        }
    }
    assert_eq!(
        violations, 0,
        "{violations} of {iterations} awaited abnormal disconnects sent DISCONNECT"
    );
}
