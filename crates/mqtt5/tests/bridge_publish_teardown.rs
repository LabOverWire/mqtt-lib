#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use mqtt5_protocol::packet::publish::PublishPacket;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn silent_remote() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                if stream
                    .write_all(&[0x20, 0x03, 0x00, 0x00, 0x00])
                    .await
                    .is_err()
                {
                    return;
                }
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            });
        }
    });
    port
}

fn alive_tasks() -> i64 {
    i64::try_from(
        tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks(),
    )
    .unwrap()
}

async fn tasks_left_after_teardown(forward: bool) -> i64 {
    let port = silent_remote().await;
    let mut config = BridgeConfig::new(
        if forward {
            "teardown-forwarding"
        } else {
            "teardown-idle"
        },
        format!("127.0.0.1:{port}"),
    )
    .add_topic("out/#", BridgeDirection::Out, QoS::AtLeastOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    for _ in 0..200 {
        if bridge.get_stats().await.connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(bridge.get_stats().await.connected, "bridge connected");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = alive_tasks();

    if forward {
        for i in 0..5 {
            let packet = PublishPacket::new("out/x", format!("{i}").into_bytes(), QoS::AtLeastOnce);
            bridge.forward_message(&packet);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    bridge.stop().await.unwrap();
    task.abort();
    let _ = task.await;
    drop(bridge);
    tokio::time::sleep(Duration::from_secs(3)).await;
    alive_tasks() - before
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_tasks_end_once_a_stopped_bridge_is_dropped() {
    let control = tasks_left_after_teardown(false).await;
    let forwarded = tasks_left_after_teardown(true).await;
    assert_eq!(
        forwarded, control,
        "tasks still alive after stop + drop: {forwarded} with 5 in-flight messages vs {control} without"
    );
}
