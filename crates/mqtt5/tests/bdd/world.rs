use cucumber::World;
use std::collections::HashMap;
use std::process::Child;
use tokio::task::JoinHandle;

use crate::common::cli_helpers::CliResult;
use crate::common::TestBroker;

#[derive(Debug, World)]
#[world(init = Self::new)]
pub struct BddWorld {
    pub broker: Option<TestBroker>,
    pub broker_tls: Option<TestBroker>,
    pub broker_url: Option<String>,
    pub last_pub_result: Option<CliResult>,
    pub last_sub_result: Option<CliResult>,
    pub pending_sub_handles: HashMap<String, JoinHandle<CliResult>>,
    pub sub_processes: Vec<Child>,
    pub expected_message_count: u32,
    pub qos: u8,
    pub retained: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    pub client_id: Option<String>,
}

impl BddWorld {
    pub fn new() -> Self {
        Self {
            broker: None,
            broker_tls: None,
            broker_url: None,
            last_pub_result: None,
            last_sub_result: None,
            pending_sub_handles: HashMap::new(),
            sub_processes: Vec::new(),
            expected_message_count: 1,
            qos: 0,
            retained: false,
            username: None,
            password: None,
            client_id: None,
        }
    }

    pub fn broker_url(&self) -> &str {
        self.broker_url
            .as_ref()
            .expect("Broker not started - use 'Given a broker is running' first")
    }

    pub async fn wait_for_subscriber(&mut self, key: &str) -> CliResult {
        if let Some(handle) = self.pending_sub_handles.remove(key) {
            match tokio::time::timeout(std::time::Duration::from_secs(5), handle).await {
                Ok(Ok(result)) => {
                    self.last_sub_result = Some(result.clone());
                    result
                }
                Ok(Err(e)) => panic!("Subscribe task failed: {e}"),
                Err(_) => panic!("Timeout waiting for subscriber"),
            }
        } else {
            panic!("No pending subscriber with key: {key}");
        }
    }

    pub fn check_subscriber_finished(&self, key: &str) -> bool {
        if let Some(handle) = self.pending_sub_handles.get(key) {
            handle.is_finished()
        } else {
            false
        }
    }

    pub fn kill_sub_processes(&mut self) {
        for mut proc in self.sub_processes.drain(..) {
            let _ = proc.kill();
        }
    }
}

impl Drop for BddWorld {
    fn drop(&mut self) {
        self.kill_sub_processes();
    }
}
