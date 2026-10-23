use mqtt5_conformance::harness::unique_client_id;
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use mqtt5_conformance::sut::inprocess_sut;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn disconnect_valid_reason_codes_accepted() {
    let sut = inprocess_sut().await;
    for &reason in &[0x00u8, 0x04, 0x80] {
        let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
            .await
            .unwrap();
        let client_id = unique_client_id(&format!("disc-rc-{reason:02x}"));
        raw.connect_and_establish(&client_id, TIMEOUT).await;

        let packet = if reason == 0x00 {
            RawPacketBuilder::disconnect_normal()
        } else {
            RawPacketBuilder::disconnect_with_reason(reason)
        };
        raw.send_raw(&packet).await.unwrap();

        assert!(
            raw.expect_disconnect(TIMEOUT).await,
            "broker must accept valid reason code 0x{reason:02x} and close connection"
        );
    }
}

#[tokio::test]
async fn no_packets_after_client_disconnect() {
    let sut = inprocess_sut().await;
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("disc-no-pkt");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::disconnect_normal())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let _ = raw.send_raw(&RawPacketBuilder::pingreq()).await;

    assert!(
        !raw.expect_pingresp(Duration::from_secs(1)).await,
        "no PINGRESP should be received after client sent DISCONNECT"
    );
}
