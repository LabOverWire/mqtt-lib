use super::common::{packets_until_quiet, start, Session, TIMEOUT};
use super::section3_disconnect::DISCONNECT_REASON_CODES;
use crate::client_conformance_test;
use crate::client_sut::{AuthSpec, ClientSutHandle, ConnectSpec};
use crate::scripted_broker::{auth, property_string, AUTH, DISCONNECT, PROP_AUTHENTICATION_METHOD};
use std::time::Duration;

const AUTH_REASON_CODES: [u8; 3] = [0x00, 0x18, 0x19];

async fn reauthenticating(
    sut: &ClientSutHandle,
    client_id: &str,
    abort_after_challenges: Option<u32>,
) -> Session {
    let spec = ConnectSpec::new(client_id).with_authentication(AuthSpec {
        method: "TEST".to_owned(),
        initial_response: Some(b"init".to_vec()),
        challenge_response: b"resp".to_vec(),
        abort_after_challenges,
    });
    let mut s = start(
        sut,
        spec,
        false,
        &property_string(PROP_AUTHENTICATION_METHOD, b"TEST"),
    )
    .await;
    s.result.as_ref().unwrap();
    s.client.reauthenticate().await.unwrap();
    let reauth = s
        .conn
        .next_of_type(AUTH, TIMEOUT)
        .await
        .expect("no re-authentication AUTH");
    assert_eq!(
        reauth.body().first(),
        Some(&0x19),
        "[MQTT-3.15.2-1] re-authentication AUTH reason code"
    );
    s
}

/// [MQTT-3.15.1-1] An AUTH with reserved flags set is malformed: the Client
/// closes the Network Connection.
#[client_conformance_test(ids = ["MQTT-3.15.1-1"], requires = ["client.enhanced_auth"])]
async fn auth_with_reserved_flags_closes_connection(sut: ClientSutHandle) {
    let mut s = reauthenticating(&sut, "auth-flags", None).await;
    let mut challenge = auth(0x18, b"TEST", &[]);
    challenge[0] = 0xF1;
    s.conn.send(&challenge).await.unwrap();
    let close = s.conn.observe_close(Duration::from_secs(5)).await;
    assert!(
        close.closed,
        "[MQTT-3.15.1-1] an AUTH with reserved flags 0x1 did not close the network connection within 5s (client is_connected={}, DISCONNECT reason {:?})",
        s.client.is_connected().await,
        close.disconnect_reason
    );
}

/// [MQTT-3.15.2-1] [MQTT-3.14.2-1] Through a re-authentication that continues
/// once and is then aborted by the Client, every AUTH carries an Authenticate
/// Reason Code and the DISCONNECT a DISCONNECT Reason Code.
#[client_conformance_test(
    ids = ["MQTT-3.15.2-1", "MQTT-3.14.2-1"],
    requires = ["client.enhanced_auth"],
)]
async fn reauthentication_uses_auth_and_disconnect_reason_codes(sut: ClientSutHandle) {
    let mut s = reauthenticating(&sut, "auth-codes", Some(1)).await;
    s.conn.send(&auth(0x18, b"TEST", &[])).await.unwrap();
    let answer = s
        .conn
        .next_of_type(AUTH, TIMEOUT)
        .await
        .expect("the client did not answer the first challenge");
    assert_eq!(
        answer.body().first(),
        Some(&0x18),
        "[MQTT-3.15.2-1] AUTH answering a challenge carries Continue authentication"
    );
    s.conn.send(&auth(0x18, b"TEST", &[])).await.unwrap();
    let after = packets_until_quiet(&mut s.conn, Duration::from_secs(3)).await;
    let reasons: Vec<(u8, u8)> = after
        .iter()
        .map(|p| (p.packet_type(), p.body().first().copied().unwrap_or(0x00)))
        .collect();
    assert!(
        reasons.iter().any(|(t, _)| *t == DISCONNECT),
        "the aborted re-authentication sent no DISCONNECT: {reasons:?}"
    );
    for (packet_type, reason) in &reasons {
        match *packet_type {
            AUTH => assert!(
                AUTH_REASON_CODES.contains(reason),
                "[MQTT-3.15.2-1] AUTH reason code 0x{reason:02X} is not an Authenticate Reason Code"
            ),
            DISCONNECT => assert!(
                DISCONNECT_REASON_CODES.contains(reason),
                "[MQTT-3.14.2-1] DISCONNECT reason code 0x{reason:02X} is not a DISCONNECT Reason Code"
            ),
            _ => {}
        }
    }
}
