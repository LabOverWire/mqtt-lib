use mqtt5_conformance::harness::unique_client_id;
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use mqtt5_conformance::sut::inprocess_sut;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn max_length_topic_handled() {
    let sut = inprocess_sut().await;
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("bigtopic");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_with_oversized_topic())
        .await
        .unwrap();

    let response = raw.expect_any_packet(TIMEOUT).await;
    match response {
        Some(data) if !data.is_empty() && data[0] == 0xE0 => {}
        Some(_) | None => {
            raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
            let alive = raw.expect_pingresp(TIMEOUT).await;
            assert!(
                alive || raw.expect_disconnect(Duration::from_millis(500)).await,
                "broker must handle max-length topic without hanging"
            );
        }
    }
}
