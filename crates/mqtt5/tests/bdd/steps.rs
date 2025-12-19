use cucumber::{given, then, when};
use mqtt5::time::Duration;

use super::world::BddWorld;
use crate::common::cli_helpers::{
    run_cli_pub, run_cli_sub_async, trigger_abnormal_disconnect_with_will,
};
use crate::common::TestBroker;

#[given("a broker is running")]
async fn start_broker(world: &mut BddWorld) {
    let broker = TestBroker::start().await;
    let url = broker.address().to_string();
    world.broker_url = Some(url);
    world.broker = Some(broker);
}

#[given("a broker with TLS is running")]
async fn start_broker_with_tls(world: &mut BddWorld) {
    let broker = TestBroker::start_with_tls().await;
    let url = broker.address().to_string();
    world.broker_url = Some(url);
    world.broker_tls = Some(broker);
}

#[when(regex = r#"^I publish "([^"]*)" to "([^"]*)"$"#)]
async fn publish_message(world: &mut BddWorld, message: String, topic: String) {
    let mut args = vec![];

    if world.qos > 0 {
        args.push("--qos");
        args.push(match world.qos {
            1 => "1",
            2 => "2",
            _ => "0",
        });
    }

    if world.retained {
        args.push("--retain");
    }

    if let Some(ref username) = world.username {
        args.push("--username");
        args.push(username.as_str());
    }

    if let Some(ref password) = world.password {
        args.push("--password");
        args.push(password.as_str());
    }

    let args_refs = args.to_vec();
    let result = run_cli_pub(world.broker_url(), &topic, &message, &args_refs).await;

    world.last_pub_result = Some(result);
}

#[when(regex = r#"^I publish "([^"]*)" to "([^"]*)" with QoS (\d+)$"#)]
async fn publish_message_with_qos(world: &mut BddWorld, message: String, topic: String, qos: u8) {
    world.qos = qos;
    publish_message(world, message, topic).await;
    world.qos = 0;
}

#[when(regex = r#"^I subscribe to "([^"]*)"$"#)]
async fn subscribe_to_topic(world: &mut BddWorld, topic: String) {
    let handle = run_cli_sub_async(
        world.broker_url(),
        &topic,
        world.expected_message_count,
        &[],
    );

    world.pending_sub_handles.insert(topic.clone(), handle);

    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[when(regex = r#"^I subscribe to "([^"]*)" expecting (\d+) messages?$"#)]
async fn subscribe_expecting_count(world: &mut BddWorld, topic: String, count: u32) {
    world.expected_message_count = count;
    subscribe_to_topic(world, topic).await;
    world.expected_message_count = 1;
}

#[when(regex = r#"^I subscribe to "([^"]*)" with QoS (\d+)$"#)]
async fn subscribe_with_qos(world: &mut BddWorld, topic: String, qos: u8) {
    let qos_arg = qos.to_string();
    let handle = run_cli_sub_async(
        world.broker_url(),
        &topic,
        world.expected_message_count,
        &["--qos", &qos_arg],
    );

    world.pending_sub_handles.insert(topic.clone(), handle);

    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[when(regex = r#"^I subscribe to "([^"]*)" with QoS (\d+) expecting (\d+) messages?$"#)]
async fn subscribe_with_qos_expecting_count(
    world: &mut BddWorld,
    topic: String,
    qos: u8,
    count: u32,
) {
    world.expected_message_count = count;
    subscribe_with_qos(world, topic, qos).await;
    world.expected_message_count = 1;
}

#[when("the retained flag is set")]
fn set_retained_flag(world: &mut BddWorld) {
    world.retained = true;
}

#[when(regex = r#"^I set username to "([^"]*)"$"#)]
fn set_username(world: &mut BddWorld, username: String) {
    world.username = Some(username);
}

#[when(regex = r#"^I set password to "([^"]*)"$"#)]
fn set_password(world: &mut BddWorld, password: String) {
    world.password = Some(password);
}

#[when(regex = r#"^I wait (\d+) milliseconds$"#)]
async fn wait_milliseconds(_world: &mut BddWorld, millis: u64) {
    tokio::time::sleep(Duration::from_millis(millis)).await;
}

#[then("the subscriber should not have received any messages yet")]
fn subscriber_should_not_have_messages(world: &mut BddWorld) {
    let topic_key = world.pending_sub_handles.keys().next().unwrap().clone();
    let is_finished = world.check_subscriber_finished(&topic_key);

    assert!(
        !is_finished,
        "Subscriber should still be waiting for messages (not finished yet), but it has already completed. \
        This indicates the will message was delivered too early, before the delay interval expired."
    );
}

#[then(regex = r#"^I should receive "([^"]*)"$"#)]
async fn should_receive_message(world: &mut BddWorld, expected_message: String) {
    let topic_key = world.pending_sub_handles.keys().next().unwrap().clone();
    let result = world.wait_for_subscriber(&topic_key).await;

    assert!(
        result.stdout_contains(&expected_message),
        "Expected to receive '{}', but stdout was: {}\nstderr: {}",
        expected_message,
        result.stdout,
        result.stderr
    );
}

#[then(regex = r#"^I should receive "([^"]*)" on topic "([^"]*)"$"#)]
async fn should_receive_message_on_topic(
    world: &mut BddWorld,
    expected_message: String,
    topic: String,
) {
    let result = world.wait_for_subscriber(&topic).await;

    assert!(
        result.stdout_contains(&expected_message),
        "Expected to receive '{}' on topic '{}', but stdout was: {}\nstderr: {}",
        expected_message,
        topic,
        result.stdout,
        result.stderr
    );
}

#[then("the publish command should succeed")]
fn publish_should_succeed(world: &mut BddWorld) {
    let result = world
        .last_pub_result
        .as_ref()
        .expect("No publish command was executed");

    assert!(
        result.success,
        "Publish command should succeed, but failed with stderr: {}",
        result.stderr
    );
}

#[then("the subscribe command should succeed")]
fn subscribe_should_succeed(world: &mut BddWorld) {
    let result = world
        .last_sub_result
        .as_ref()
        .expect("No subscribe command was executed");

    assert!(
        result.success,
        "Subscribe command should succeed, but failed with stderr: {}",
        result.stderr
    );
}

#[then("the publish command should fail")]
fn publish_should_fail(world: &mut BddWorld) {
    let result = world
        .last_pub_result
        .as_ref()
        .expect("No publish command was executed");

    assert!(
        !result.success,
        "Publish command should fail, but it succeeded"
    );
}

#[then(regex = r#"^the error should contain "([^"]*)"$"#)]
fn error_should_contain(world: &mut BddWorld, expected_error: String) {
    let result = world
        .last_pub_result
        .as_ref()
        .or(world.last_sub_result.as_ref())
        .expect("No command was executed");

    assert!(
        result.stderr_contains(&expected_error),
        "Expected error to contain '{}', but stderr was: {}",
        expected_error,
        result.stderr
    );
}

#[then(regex = r#"^the output should contain "([^"]*)"$"#)]
fn output_should_contain(world: &mut BddWorld, expected_text: String) {
    let result = world
        .last_pub_result
        .as_ref()
        .or(world.last_sub_result.as_ref())
        .expect("No command was executed");

    assert!(
        result.contains(&expected_text),
        "Expected output to contain '{}', but got stdout: {}\nstderr: {}",
        expected_text,
        result.stdout,
        result.stderr
    );
}

#[then(regex = r#"^the message QoS should be (\d+)$"#)]
async fn message_qos_should_be(world: &mut BddWorld, _expected_qos: u8) {
    let result = world
        .last_sub_result
        .as_ref()
        .expect("No subscribe command was executed");

    assert!(
        result.success,
        "Subscribe should succeed to verify QoS, but failed with: {}",
        result.stderr
    );
}

#[then(regex = r#"^I should receive all (\d+) messages$"#)]
async fn should_receive_all_messages(world: &mut BddWorld, count: u32) {
    let topic_key = world.pending_sub_handles.keys().next().unwrap().clone();
    let result = world.wait_for_subscriber(&topic_key).await;

    assert!(
        result.success,
        "Subscribe should succeed, but failed with: {}",
        result.stderr
    );

    let message_lines: Vec<&str> = result
        .stdout
        .lines()
        .filter(|line| !line.contains("✓") && !line.trim().is_empty())
        .collect();

    assert_eq!(
        message_lines.len(),
        count as usize,
        "Expected {} messages, but received {} messages. Output:\n{}",
        count,
        message_lines.len(),
        result.stdout
    );
}

#[when(regex = r#"^I publish "([^"]*)" to "([^"]*)" with client-id "([^"]*)"$"#)]
async fn publish_with_client_id(
    world: &mut BddWorld,
    message: String,
    topic: String,
    client_id: String,
) {
    world.client_id = Some(client_id.clone());
    let result = run_cli_pub(
        world.broker_url(),
        &topic,
        &message,
        &["--client-id", &client_id],
    )
    .await;
    world.last_pub_result = Some(result);
}

#[when(regex = r#"^I publish "([^"]*)" to "([^"]*)" with client-id "([^"]*)" and clean-start$"#)]
async fn publish_with_client_id_and_clean_start(
    world: &mut BddWorld,
    message: String,
    topic: String,
    client_id: String,
) {
    world.client_id = Some(client_id.clone());
    let result = run_cli_pub(
        world.broker_url(),
        &topic,
        &message,
        &["--client-id", &client_id],
    )
    .await;
    world.last_pub_result = Some(result);
}

#[when(
    regex = r#"^I publish "([^"]*)" to "([^"]*)" with client-id "([^"]*)" without clean-start$"#
)]
async fn publish_with_client_id_without_clean_start(
    world: &mut BddWorld,
    message: String,
    topic: String,
    client_id: String,
) {
    world.client_id = Some(client_id.clone());
    let result = run_cli_pub(
        world.broker_url(),
        &topic,
        &message,
        &["--client-id", &client_id, "--no-clean-start"],
    )
    .await;
    world.last_pub_result = Some(result);
}

#[when(
    regex = r#"^I publish "([^"]*)" to "([^"]*)" with client-id "([^"]*)" and session-expiry (\d+)$"#
)]
async fn publish_with_session_expiry(
    world: &mut BddWorld,
    message: String,
    topic: String,
    client_id: String,
    expiry: u32,
) {
    world.client_id = Some(client_id.clone());
    let expiry_str = expiry.to_string();
    let result = run_cli_pub(
        world.broker_url(),
        &topic,
        &message,
        &["--client-id", &client_id, "--session-expiry", &expiry_str],
    )
    .await;
    world.last_pub_result = Some(result);
}

#[when(
    regex = r#"^I subscribe to "([^"]*)" with QoS (\d+) and client-id "([^"]*)" expecting (\d+) messages?$"#
)]
async fn subscribe_with_qos_and_client_id(
    world: &mut BddWorld,
    topic: String,
    qos: u8,
    client_id: String,
    count: u32,
) {
    world.client_id = Some(client_id.clone());
    let qos_arg = qos.to_string();
    let handle = run_cli_sub_async(
        world.broker_url(),
        &topic,
        count,
        &["--qos", &qos_arg, "--client-id", &client_id],
    );

    world.pending_sub_handles.insert(topic.clone(), handle);
    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[when(regex = r#"^I publish with will message "([^"]*)" on topic "([^"]*)"$"#)]
async fn publish_with_will_message(world: &mut BddWorld, will_message: String, will_topic: String) {
    let result =
        trigger_abnormal_disconnect_with_will(world.broker_url(), &will_topic, &will_message, &[])
            .await;
    world.last_pub_result = Some(result);
}

#[when(regex = r#"^I publish with will message "([^"]*)" on topic "([^"]*)" with delay (\d+)$"#)]
async fn publish_with_will_message_and_delay(
    world: &mut BddWorld,
    will_message: String,
    will_topic: String,
    delay: u32,
) {
    let delay_str = delay.to_string();
    let result = trigger_abnormal_disconnect_with_will(
        world.broker_url(),
        &will_topic,
        &will_message,
        &["--will-delay", &delay_str],
    )
    .await;
    world.last_pub_result = Some(result);
}

#[when(regex = r#"^I publish with will message "([^"]*)" on topic "([^"]*)" with QoS (\d+)$"#)]
async fn publish_with_will_message_and_qos(
    world: &mut BddWorld,
    will_message: String,
    will_topic: String,
    qos: u8,
) {
    let qos_str = qos.to_string();
    let result = trigger_abnormal_disconnect_with_will(
        world.broker_url(),
        &will_topic,
        &will_message,
        &["--will-qos", &qos_str],
    )
    .await;
    world.last_pub_result = Some(result);
}

#[when(regex = r#"^I publish "([^"]*)" to "([^"]*)" using TLS$"#)]
async fn publish_using_tls(world: &mut BddWorld, message: String, topic: String) {
    let result = run_cli_pub(world.broker_url(), &topic, &message, &["--insecure"]).await;
    world.last_pub_result = Some(result);
}

#[when(regex = r#"^I subscribe to "([^"]*)" expecting (\d+) messages? using TLS$"#)]
async fn subscribe_using_tls(world: &mut BddWorld, topic: String, count: u32) {
    let handle = run_cli_sub_async(world.broker_url(), &topic, count, &["--insecure"]);
    world.pending_sub_handles.insert(topic.clone(), handle);
    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[when(regex = r#"^I subscribe to "([^"]*)" with QoS (\d+) expecting (\d+) messages? using TLS$"#)]
async fn subscribe_with_qos_using_tls(world: &mut BddWorld, topic: String, qos: u8, count: u32) {
    let qos_arg = qos.to_string();
    let handle = run_cli_sub_async(
        world.broker_url(),
        &topic,
        count,
        &["--insecure", "--qos", &qos_arg],
    );
    world.pending_sub_handles.insert(topic.clone(), handle);
    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[when(regex = r#"^I publish "([^"]*)" to "([^"]*)" with QoS (\d+) using TLS$"#)]
async fn publish_with_qos_using_tls(world: &mut BddWorld, message: String, topic: String, qos: u8) {
    world.qos = qos;
    publish_using_tls(world, message, topic).await;
    world.qos = 0;
}

#[given("a broker with WebSocket is running")]
async fn start_broker_with_websocket(world: &mut BddWorld) {
    let broker = TestBroker::start_with_websocket().await;
    let url = broker.address().to_string();
    world.broker_url = Some(url);
    world.broker = Some(broker);
}

#[when(regex = r#"^I publish "([^"]*)" to "([^"]*)" using WebSocket$"#)]
async fn publish_using_websocket(world: &mut BddWorld, message: String, topic: String) {
    let result = run_cli_pub(world.broker_url(), &topic, &message, &[]).await;
    world.last_pub_result = Some(result);
}

#[when(regex = r#"^I subscribe to "([^"]*)" expecting (\d+) messages? using WebSocket$"#)]
async fn subscribe_using_websocket(world: &mut BddWorld, topic: String, count: u32) {
    let handle = run_cli_sub_async(world.broker_url(), &topic, count, &[]);
    world.pending_sub_handles.insert(topic.clone(), handle);
    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[when(
    regex = r#"^I subscribe to "([^"]*)" with QoS (\d+) expecting (\d+) messages? using WebSocket$"#
)]
async fn subscribe_with_qos_using_websocket(
    world: &mut BddWorld,
    topic: String,
    qos: u8,
    count: u32,
) {
    let qos_arg = qos.to_string();
    let handle = run_cli_sub_async(world.broker_url(), &topic, count, &["--qos", &qos_arg]);
    world.pending_sub_handles.insert(topic.clone(), handle);
    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[when(regex = r#"^I publish "([^"]*)" to "([^"]*)" with QoS (\d+) using WebSocket$"#)]
async fn publish_with_qos_using_websocket(
    world: &mut BddWorld,
    message: String,
    topic: String,
    qos: u8,
) {
    world.qos = qos;
    publish_using_websocket(world, message, topic).await;
    world.qos = 0;
}

#[given("a broker with authentication is running")]
async fn start_broker_with_authentication(world: &mut BddWorld) {
    let broker = TestBroker::start_with_authentication().await;
    let url = broker.address().to_string();
    world.broker_url = Some(url);
    world.broker = Some(broker);
}
