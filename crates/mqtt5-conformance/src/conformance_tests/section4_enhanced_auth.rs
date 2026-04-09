//! Section 4.12 — Enhanced Authentication (tests that work against the
//! default `AllowAllAuth` provider).
//!
//! Tests that require a pre-configured `CHALLENGE-RESPONSE` provider remain
//! in `tests/section4_enhanced_auth.rs` because they need to inject an
//! `mqtt5::broker::auth::AuthProvider` implementation at SUT construction
//! time, which is outside the scope of the vendor-neutral `SutHandle`.

use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use std::time::Duration;

/// `[MQTT-4.12.0-1]` If the Server does not support the Authentication
/// Method supplied by the Client, it MAY send a CONNACK with a Reason
/// Code of 0x8C (Bad Authentication Method).
#[conformance_test(
    ids = ["MQTT-4.12.0-1"],
    requires = ["transport.tcp"],
)]
async fn unsupported_auth_method_closes(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("unsup-auth");
    let connect = RawPacketBuilder::connect_with_auth_method(&client_id, "UNKNOWN-METHOD");
    raw.send_raw(&connect).await.unwrap();

    let connack = raw.expect_connack(Duration::from_secs(3)).await;
    assert!(
        connack.is_some(),
        "[MQTT-4.12.0-1] Server must send CONNACK for unsupported auth method"
    );
    let (_, reason_code) = connack.unwrap();
    assert_eq!(
        reason_code, 0x8C,
        "[MQTT-4.12.0-1] CONNACK reason code must be 0x8C (Bad Authentication Method)"
    );

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-4.12.0-1] Server must close connection after rejecting auth method"
    );
}

/// `[MQTT-4.12.0-6]` If the Client does not include an Authentication
/// Method in the CONNECT, the Server MUST NOT send an AUTH packet.
#[conformance_test(
    ids = ["MQTT-4.12.0-6"],
    requires = ["transport.tcp"],
)]
async fn no_auth_method_no_server_auth(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("no-auth");
    let connect = RawPacketBuilder::valid_connect(&client_id);
    raw.send_raw(&connect).await.unwrap();

    let connack = raw.expect_connack(Duration::from_secs(3)).await;
    assert!(
        connack.is_some(),
        "[MQTT-4.12.0-6] Server must send CONNACK for plain connect"
    );
    let (_, reason_code) = connack.unwrap();
    assert_eq!(
        reason_code, 0x00,
        "[MQTT-4.12.0-6] CONNACK must be success for plain connect"
    );

    raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
    assert!(
        raw.expect_pingresp(Duration::from_secs(3)).await,
        "[MQTT-4.12.0-6] Connection must remain operational with no AUTH packets sent"
    );
}

/// `[MQTT-4.12.0-7]` If the Client does not include an Authentication
/// Method in the CONNECT, the Client MUST NOT send an AUTH packet. The
/// Server treats this as a Protocol Error.
#[conformance_test(
    ids = ["MQTT-4.12.0-7"],
    requires = ["transport.tcp"],
)]
async fn unsolicited_auth_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("unsol-auth");
    raw.connect_and_establish(&client_id, Duration::from_secs(3))
        .await;

    let auth = RawPacketBuilder::auth_with_method(0x19, "SOME-METHOD");
    raw.send_raw(&auth).await.unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(3)).await,
        "[MQTT-4.12.0-7] Server must disconnect client that sends AUTH without prior auth method"
    );
}
