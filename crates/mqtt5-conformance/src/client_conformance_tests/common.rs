use crate::client_sut::{
    ClientError, ClientFuture, ClientSutHandle, ClientUnderTest, ConnectOutcome, ConnectSpec,
    PublishOutcome, PublishSpec, ReceivedMessage,
};
use crate::packet_parser::ParsedPublish;
use crate::scripted_broker::{
    connack, property_u16, subscription_ack, BrokerConnection, CloseObservation, Next, Packet,
    ScriptedBroker, CONNECT, PROP_RECEIVE_MAXIMUM, PUBLISH, SUBSCRIBE,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const SHORT: Duration = Duration::from_millis(600);

pub type ConnectResult = Result<ConnectOutcome, ClientError>;

pub struct Pending {
    pub client: Arc<dyn ClientUnderTest>,
    pub broker: ScriptedBroker,
    pub conn: BrokerConnection,
    pub connecting: JoinHandle<ConnectResult>,
}

pub struct Session {
    pub client: Arc<dyn ClientUnderTest>,
    pub broker: ScriptedBroker,
    pub conn: BrokerConnection,
    pub connect: Packet,
    pub result: ConnectResult,
}

pub async fn begin(sut: &ClientSutHandle, spec: ConnectSpec) -> Pending {
    let broker = ScriptedBroker::bind()
        .await
        .expect("bind the scripted broker");
    let url = broker.url().expect("scripted broker address");
    let client = sut.create(spec);
    let connecting = tokio::spawn(client.connect(&url));
    let conn = broker
        .accept(TIMEOUT)
        .await
        .expect("client did not open a connection");
    Pending {
        client,
        broker,
        conn,
        connecting,
    }
}

pub async fn read_connect(conn: &mut BrokerConnection) -> Packet {
    match conn.next(TIMEOUT).await {
        Next::Packet(packet) if packet.packet_type() == CONNECT => packet,
        _ => panic!("client did not send CONNECT as its first packet"),
    }
}

pub async fn finish(connecting: JoinHandle<ConnectResult>) -> ConnectResult {
    tokio::time::timeout(TIMEOUT, connecting)
        .await
        .expect("connect did not complete")
        .expect("connect task panicked")
}

pub async fn start(
    sut: &ClientSutHandle,
    spec: ConnectSpec,
    session_present: bool,
    props: &[u8],
) -> Session {
    let Pending {
        client,
        broker,
        mut conn,
        connecting,
    } = begin(sut, spec).await;
    let connect = read_connect(&mut conn).await;
    conn.send(&connack(session_present, props))
        .await
        .expect("send CONNACK");
    let result = finish(connecting).await;
    Session {
        client,
        broker,
        conn,
        connect,
        result,
    }
}

pub async fn accept_next(
    broker: &ScriptedBroker,
    session_present: bool,
    props: &[u8],
) -> (BrokerConnection, Packet) {
    let mut conn = broker
        .accept(Duration::from_secs(8))
        .await
        .expect("client did not reconnect");
    let connect = read_connect(&mut conn).await;
    conn.send(&connack(session_present, props))
        .await
        .expect("send CONNACK");
    (conn, connect)
}

pub async fn wait_connected(client: &Arc<dyn ClientUnderTest>, want: bool) {
    let reached = tokio::time::timeout(Duration::from_secs(5), async {
        while client.is_connected().await != want {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        reached.is_ok(),
        "client connected state never became {want}"
    );
}

pub async fn drop_connection(session: Session) -> (Arc<dyn ClientUnderTest>, ScriptedBroker) {
    let Session {
        client,
        broker,
        conn,
        ..
    } = session;
    drop(conn);
    wait_connected(&client, false).await;
    (client, broker)
}

pub async fn reconnect_with(
    sut: &ClientSutHandle,
    client_id: &str,
    first_props: &[u8],
    second_props: &[u8],
) -> (Arc<dyn ClientUnderTest>, ScriptedBroker, BrokerConnection) {
    let session = start(
        sut,
        ConnectSpec::new(client_id).reconnecting(),
        false,
        first_props,
    )
    .await;
    session.result.as_ref().expect("first connect");
    let (client, broker) = drop_connection(session).await;
    let (conn, _) = accept_next(&broker, false, second_props).await;
    wait_connected(&client, true).await;
    (client, broker, conn)
}

pub async fn queued_publish_then_reconnect(
    sut: &ClientSutHandle,
    client_id: &str,
    second_props: &[u8],
    spec: PublishSpec,
) -> Option<ParsedPublish> {
    let session = start(
        sut,
        ConnectSpec::new(client_id).reconnecting().resuming(300),
        false,
        &[],
    )
    .await;
    session.result.as_ref().expect("first connect");
    let (client, broker) = drop_connection(session).await;
    let queued = client.publish("queued/t", b"offline", spec).await;
    assert!(
        queued.is_ok(),
        "publish while offline should queue: {queued:?}"
    );
    let (mut conn, _) = accept_next(&broker, true, second_props).await;
    let publish = conn
        .next_of_type(PUBLISH, Duration::from_secs(3))
        .await
        .map(|packet| parse_publish(&packet));
    publish.inspect(|p| {
        assert_eq!(p.topic, "queued/t");
        assert_eq!(p.payload, b"offline");
    })
}

pub async fn packets_until_quiet(conn: &mut BrokerConnection, wait: Duration) -> Vec<Packet> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut packets = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match conn.next_answering_pings(left).await {
            Next::Packet(packet) => packets.push(packet),
            Next::Malformed | Next::Closed | Next::Silent => return packets,
        }
    }
}

pub async fn publishes_until_quiet(conn: &mut BrokerConnection, wait: Duration) -> Vec<Packet> {
    packets_until_quiet(conn, wait)
        .await
        .into_iter()
        .filter(|packet| packet.packet_type() == PUBLISH)
        .collect()
}

pub async fn ack_ids(conn: &mut BrokerConnection, packet_type: u8, count: usize) -> Vec<u16> {
    let mut ids = Vec::with_capacity(count);
    while ids.len() < count {
        let packet = conn
            .next_of_type(packet_type, TIMEOUT)
            .await
            .unwrap_or_else(|| panic!("client sent {} of {count} acknowledgements", ids.len()));
        ids.push(
            packet
                .leading_packet_id()
                .expect("acknowledgement without a packet identifier"),
        );
    }
    ids
}

pub async fn queued_flush(sut: &ClientSutHandle, client_id: &str) -> Vec<Packet> {
    let spec = ConnectSpec::new(client_id)
        .resuming(300)
        .reconnecting_after(Duration::from_millis(1500), Duration::from_millis(2000));
    let session = start(sut, spec, false, &[]).await;
    session.result.as_ref().expect("first connect");
    let (client, broker) = drop_connection(session).await;
    for i in 0..3u8 {
        let queued = client.publish("t/queued", &[i], PublishSpec::qos(1)).await;
        assert!(
            matches!(queued, Ok(PublishOutcome::Pending)),
            "publish while disconnected is queued: {queued:?}"
        );
    }
    let (mut conn, _) = accept_next(&broker, true, &property_u16(PROP_RECEIVE_MAXIMUM, 1)).await;
    publishes_until_quiet(&mut conn, Duration::from_millis(1500)).await
}

pub fn parse_publish(packet: &Packet) -> ParsedPublish {
    ParsedPublish::parse(packet.bytes()).expect("client sent a malformed PUBLISH")
}

pub async fn subscribe_and_ack(session: &mut Session, filter: &str, qos: u8) {
    let subscribing = session.client.subscribe(filter, qos);
    complete_subscribe(session, subscribing, qos).await;
}

pub async fn subscribe_deferred_and_ack(session: &mut Session, filter: &str, qos: u8) {
    let subscribing = session.client.subscribe_deferred(filter, qos);
    complete_subscribe(session, subscribing, qos).await;
}

async fn complete_subscribe(
    session: &mut Session,
    subscribing: ClientFuture<Result<(), ClientError>>,
    qos: u8,
) {
    let subscribing = tokio::spawn(subscribing);
    let subscribe = session
        .conn
        .next_of_type(SUBSCRIBE, TIMEOUT)
        .await
        .expect("client did not send SUBSCRIBE");
    let packet_id = subscribe
        .leading_packet_id()
        .expect("SUBSCRIBE without a packet identifier");
    session
        .conn
        .send(&subscription_ack(0x90, packet_id, &[], &[qos]))
        .await
        .expect("send SUBACK");
    tokio::time::timeout(TIMEOUT, subscribing)
        .await
        .expect("subscribe did not complete")
        .expect("subscribe task panicked")
        .expect("subscribe failed");
}

pub async fn wait_for_received(
    client: &Arc<dyn ClientUnderTest>,
    count: usize,
) -> Vec<ReceivedMessage> {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let received = client.received();
        if received.len() >= count || tokio::time::Instant::now() >= deadline {
            return received;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub async fn received_after(
    client: &Arc<dyn ClientUnderTest>,
    wait: Duration,
) -> Vec<ReceivedMessage> {
    tokio::time::sleep(wait).await;
    client.received()
}

pub struct MalformedOutcome {
    pub delivered: usize,
    pub still_connected: bool,
    pub close: CloseObservation,
}

pub async fn inject_after_subscribe(
    sut: &ClientSutHandle,
    client_id: &str,
    bytes: &[u8],
) -> MalformedOutcome {
    let mut session = start(sut, ConnectSpec::new(client_id), false, &[]).await;
    session.result.as_ref().expect("connect");
    subscribe_and_ack(&mut session, "#", 0).await;
    session.conn.send(bytes).await.expect("send packet");
    let close = session.conn.observe_close(Duration::from_secs(3)).await;
    MalformedOutcome {
        delivered: session.client.received().len(),
        still_connected: session.client.is_connected().await,
        close,
    }
}
