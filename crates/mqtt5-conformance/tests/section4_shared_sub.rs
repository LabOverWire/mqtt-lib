use mqtt5_conformance::harness::unique_client_id;
use mqtt5_conformance::sut::inprocess_sut;
use mqtt5_conformance::test_client::TestClient;
use mqtt5_protocol::types::{PublishOptions, SubscribeOptions};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn shared_sub_round_robin_delivery() {
    let sut = inprocess_sut().await;
    let topic = format!("tasks/{}", unique_client_id("rr"));
    let shared_filter = format!("$share/workers/{topic}");

    let worker1 = TestClient::connect_with_prefix(&sut, "shared-rr-w1")
        .await
        .unwrap();
    let sub1 = worker1
        .subscribe(&shared_filter, SubscribeOptions::default())
        .await
        .unwrap();

    let worker2 = TestClient::connect_with_prefix(&sut, "shared-rr-w2")
        .await
        .unwrap();
    let sub2 = worker2
        .subscribe(&shared_filter, SubscribeOptions::default())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "shared-rr-pub")
        .await
        .unwrap();
    for i in 0..6 {
        let payload = format!("msg-{i}");
        publisher.publish(&topic, payload.as_bytes()).await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let count1 = sub1.count();
    let count2 = sub2.count();
    assert_eq!(
        count1 + count2,
        6,
        "all 6 messages must be delivered across shared group"
    );
    assert!(
        count1 >= 1 && count2 >= 1,
        "both shared subscribers must receive at least one message: w1={count1}, w2={count2}"
    );
}

#[tokio::test]
async fn shared_sub_no_retained_on_subscribe() {
    let sut = inprocess_sut().await;
    let topic = format!("sensor/temp/{}", unique_client_id("ret"));
    let shared_filter = format!("$share/readers/{topic}");

    let publisher = TestClient::connect_with_prefix(&sut, "shared-ret-pub")
        .await
        .unwrap();
    publisher
        .publish_with_options(
            &topic,
            b"25.5",
            PublishOptions {
                retain: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let shared_sub = TestClient::connect_with_prefix(&sut, "shared-ret-s")
        .await
        .unwrap();
    let sub_shared = shared_sub
        .subscribe(&shared_filter, SubscribeOptions::default())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        sub_shared.count(),
        0,
        "shared subscription must not receive retained messages on subscribe"
    );

    let regular_sub = TestClient::connect_with_prefix(&sut, "shared-ret-r")
        .await
        .unwrap();
    let sub_regular = regular_sub
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .unwrap();

    sub_regular.wait_for_messages(1, TIMEOUT).await;
    let msgs = sub_regular.snapshot();
    assert_eq!(
        msgs.len(),
        1,
        "regular subscription must receive retained message"
    );
    assert_eq!(msgs[0].payload, b"25.5");
}
