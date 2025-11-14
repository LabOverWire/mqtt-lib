#![allow(dead_code)]

use mqtt5::time::Duration;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use tokio::time::timeout;

const CLI_BINARY: &str = "target/release/mqttv5";

#[derive(Debug, Clone)]
pub struct CliResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

impl CliResult {
    pub fn from_output(output: &Output) -> Self {
        Self {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            success: output.status.success(),
        }
    }

    pub fn contains(&self, text: &str) -> bool {
        self.stdout.contains(text) || self.stderr.contains(text)
    }

    pub fn stdout_contains(&self, text: &str) -> bool {
        self.stdout.contains(text)
    }

    pub fn stderr_contains(&self, text: &str) -> bool {
        self.stderr.contains(text)
    }
}

pub fn ensure_cli_built() {
    if !std::path::Path::new(CLI_BINARY).exists() {
        let output = Command::new("cargo")
            .args(["build", "--release", "-p", "mqttv5-cli"])
            .output()
            .expect("Failed to build CLI");

        assert!(
            output.status.success(),
            "Failed to build CLI: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

pub async fn run_cli_command(args: &[&str]) -> CliResult {
    ensure_cli_built();

    let output = tokio::task::spawn_blocking({
        let args = args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        move || {
            Command::new(CLI_BINARY)
                .args(&args)
                .output()
                .expect("Failed to run CLI")
        }
    })
    .await
    .expect("Failed to join task");

    CliResult::from_output(&output)
}

pub async fn run_cli_pub(
    broker_url: &str,
    topic: &str,
    message: &str,
    extra_args: &[&str],
) -> CliResult {
    let mut args = vec![
        "pub",
        "--url",
        broker_url,
        "--topic",
        topic,
        "--message",
        message,
        "--non-interactive",
    ];
    args.extend_from_slice(extra_args);
    run_cli_command(&args).await
}

#[allow(clippy::unused_async)]
pub async fn run_cli_sub_async(
    broker_url: &str,
    topic: &str,
    count: u32,
    extra_args: &[&str],
) -> tokio::task::JoinHandle<CliResult> {
    ensure_cli_built();

    let broker_url = broker_url.to_string();
    let topic = topic.to_string();
    let extra_args = extra_args
        .iter()
        .map(|s| (*s).to_string())
        .collect::<Vec<_>>();

    tokio::task::spawn_blocking(move || {
        let mut args = vec![
            "sub".to_string(),
            "--url".to_string(),
            broker_url,
            "--topic".to_string(),
            topic,
            "--count".to_string(),
            count.to_string(),
            "--non-interactive".to_string(),
        ];
        for arg in extra_args {
            args.push(arg);
        }

        let output = Command::new(CLI_BINARY)
            .args(&args)
            .output()
            .expect("Failed to run CLI");

        CliResult::from_output(&output)
    })
}

pub async fn verify_pub_sub_delivery(
    broker_url: &str,
    topic: &str,
    message: &str,
    extra_args: &[&str],
) -> Result<bool, String> {
    // Start subscriber first
    let sub_handle = run_cli_sub_async(broker_url, topic, 1, extra_args).await;

    // Give subscriber time to connect
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish message
    let pub_result = run_cli_pub(broker_url, topic, message, extra_args).await;
    if !pub_result.success {
        return Err(format!("Publish failed: {}", pub_result.stderr));
    }

    // Wait for subscriber to receive message
    match timeout(Duration::from_secs(3), sub_handle).await {
        Ok(Ok(sub_result)) => {
            if sub_result.stdout_contains(message) {
                Ok(true)
            } else {
                Err(format!(
                    "Message not received. Stdout: {}, Stderr: {}",
                    sub_result.stdout, sub_result.stderr
                ))
            }
        }
        Ok(Err(e)) => Err(format!("Subscribe task failed: {e}")),
        Err(_) => Err("Timeout waiting for message".to_string()),
    }
}

pub async fn verify_session_persistence(broker_url: &str, client_id: &str) -> Result<bool, String> {
    // First connection with clean start
    let result1 = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/session",
        "--message",
        "test",
        "--client-id",
        client_id,
        "--non-interactive",
    ])
    .await;

    if !result1.success {
        return Err(format!("First connection failed: {}", result1.stderr));
    }

    // Second connection without clean start - should resume session
    let result2 = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/session",
        "--message",
        "test",
        "--client-id",
        client_id,
        "--no-clean-start",
        "--non-interactive",
    ])
    .await;

    if !result2.success {
        return Err(format!("Second connection failed: {}", result2.stderr));
    }

    // Check for session resumption message
    Ok(result2.stdout_contains("Resumed existing session")
        || result2.stdout_contains("Session present: true"))
}

pub async fn trigger_abnormal_disconnect_with_will(
    broker_url: &str,
    will_topic: &str,
    will_message: &str,
    extra_args: &[&str],
) -> CliResult {
    ensure_cli_built();

    let mut args = vec![
        "pub".to_string(),
        "--url".to_string(),
        broker_url.to_string(),
        "--topic".to_string(),
        "test/alive".to_string(),
        "--message".to_string(),
        "online".to_string(),
        "--will-topic".to_string(),
        will_topic.to_string(),
        "--will-message".to_string(),
        will_message.to_string(),
        "--non-interactive".to_string(),
        "--keep-alive-after-publish".to_string(),
    ];

    for arg in extra_args {
        args.push(arg.to_string());
    }

    let mut pub_process = Command::new(CLI_BINARY)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start publisher");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = pub_process.kill();

    let output = pub_process
        .wait_with_output()
        .expect("Failed to wait for process");
    CliResult::from_output(&output)
}

pub async fn verify_will_delivery(
    broker_url: &str,
    will_topic: &str,
    will_message: &str,
) -> Result<bool, String> {
    ensure_cli_built();

    // Start subscriber for will topic
    let sub_handle = run_cli_sub_async(broker_url, will_topic, 1, &[]).await;

    // Give subscriber time to connect
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start publisher with will message and keep-alive flag
    let mut pub_process = Command::new(CLI_BINARY)
        .args([
            "pub",
            "--url",
            broker_url,
            "--topic",
            "test/alive",
            "--message",
            "online",
            "--will-topic",
            will_topic,
            "--will-message",
            will_message,
            "--will-delay",
            "0",
            "--non-interactive",
            "--keep-alive-after-publish",
        ])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start publisher: {e}"))?;

    // Give publisher time to connect
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Force kill the publisher to trigger will message
    pub_process
        .kill()
        .map_err(|e| format!("Failed to kill publisher: {e}"))?;

    // Wait for will message delivery
    match timeout(Duration::from_secs(5), sub_handle).await {
        Ok(Ok(sub_result)) => {
            if sub_result.stdout_contains(will_message) {
                Ok(true)
            } else {
                Err(format!(
                    "Will message not received. Stdout: {}",
                    sub_result.stdout
                ))
            }
        }
        Ok(Err(e)) => Err(format!("Subscribe task failed: {e}")),
        Err(_) => Err("Timeout waiting for will message".to_string()),
    }
}

pub async fn verify_qos_delivery(broker_url: &str, qos: u8) -> Result<bool, String> {
    let topic = format!("test/qos{qos}");
    let message = format!("QoS {qos} test message");

    verify_pub_sub_delivery(broker_url, &topic, &message, &["--qos", &qos.to_string()]).await
}

pub async fn verify_retained_message(
    broker_url: &str,
    topic: &str,
    message: &str,
) -> Result<bool, String> {
    // Publish retained message
    let pub_result = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        topic,
        "--message",
        message,
        "--retain",
        "--non-interactive",
    ])
    .await;

    if !pub_result.success {
        return Err(format!("Failed to publish retained: {}", pub_result.stderr));
    }

    // Verify "Message retained" output
    if !pub_result.stdout_contains("Message retained") {
        return Err("Retained flag not shown in output".to_string());
    }

    // Subscribe and verify immediate delivery
    let sub_result = run_cli_command(&[
        "sub",
        "--url",
        broker_url,
        "--topic",
        topic,
        "--count",
        "1",
        "--non-interactive",
        "--verbose",
    ])
    .await;

    if !sub_result.success {
        return Err(format!("Failed to subscribe: {}", sub_result.stderr));
    }

    if !sub_result.stdout_contains(message) {
        return Err("Retained message not received".to_string());
    }

    // Clear retained message
    let _ = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        topic,
        "--message",
        "",
        "--retain",
        "--non-interactive",
    ])
    .await;

    Ok(true)
}

pub async fn run_cli_with_stdin(args: &[&str], stdin_data: &str) -> CliResult {
    ensure_cli_built();

    let stdin_data = stdin_data.to_string();
    let args = args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();

    let result = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(CLI_BINARY)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to spawn CLI");

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(stdin_data.as_bytes())
                .expect("Failed to write to stdin");
        }

        child.wait_with_output().expect("Failed to wait for CLI")
    })
    .await
    .expect("Failed to join task");

    CliResult::from_output(&result)
}

pub async fn verify_authentication(
    broker_url: &str,
    username: &str,
    password: &str,
    should_succeed: bool,
) -> Result<bool, String> {
    let result = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/auth",
        "--message",
        "authenticated",
        "--username",
        username,
        "--password",
        password,
        "--non-interactive",
    ])
    .await;

    if should_succeed {
        if result.success {
            Ok(true)
        } else {
            Err(format!("Authentication should succeed: {}", result.stderr))
        }
    } else if !result.success {
        Ok(true)
    } else {
        Err("Authentication should have failed but succeeded".to_string())
    }
}
