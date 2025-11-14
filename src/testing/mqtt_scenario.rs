//! MQTT scenario builder for complex testing scenarios

use super::{TurmoilBrokerConfig, TurmoilClientConfig};
use std::collections::HashMap;
use crate::time::Duration;

/// Network conditions for scenario testing
#[derive(Debug, Clone)]
pub struct NetworkConditions {
    pub latency: Duration,
    pub jitter: Duration,
    pub packet_loss: f32,
    pub bandwidth: Option<u64>, // bytes per second
}

impl Default for NetworkConditions {
    fn default() -> Self {
        Self {
            latency: Duration::from_millis(0),
            jitter: Duration::from_millis(0),
            packet_loss: 0.0,
            bandwidth: None,
        }
    }
}

impl NetworkConditions {
    /// Creates ideal network conditions
    #[must_use]
    pub fn ideal() -> Self {
        Self::default()
    }

    /// Creates typical LAN conditions
    #[must_use]
    pub fn lan() -> Self {
        Self {
            latency: Duration::from_millis(1),
            jitter: Duration::from_micros(100),
            packet_loss: 0.0,
            bandwidth: Some(1_000_000_000), // 1 Gbps
        }
    }

    /// Creates typical WAN conditions
    #[must_use]
    pub fn wan() -> Self {
        Self {
            latency: Duration::from_millis(50),
            jitter: Duration::from_millis(10),
            packet_loss: 0.01,
            bandwidth: Some(100_000_000), // 100 Mbps
        }
    }

    /// Creates poor network conditions
    #[must_use]
    pub fn poor() -> Self {
        Self {
            latency: Duration::from_millis(200),
            jitter: Duration::from_millis(50),
            packet_loss: 0.05,
            bandwidth: Some(1_000_000), // 1 Mbps
        }
    }

    /// Creates mobile network conditions
    #[must_use]
    pub fn mobile() -> Self {
        Self {
            latency: Duration::from_millis(100),
            jitter: Duration::from_millis(30),
            packet_loss: 0.02,
            bandwidth: Some(10_000_000), // 10 Mbps
        }
    }
}

/// Builder for MQTT test scenarios
pub struct ScenarioBuilder {
    name: String,
    broker_config: TurmoilBrokerConfig,
    clients: Vec<TurmoilClientConfig>,
    network_conditions: HashMap<String, NetworkConditions>,
    duration: Duration,
}

impl Default for ScenarioBuilder {
    fn default() -> Self {
        Self {
            name: "test-scenario".to_string(),
            broker_config: TurmoilBrokerConfig::default(),
            clients: Vec::new(),
            network_conditions: HashMap::new(),
            duration: Duration::from_secs(30),
        }
    }
}

impl ScenarioBuilder {
    /// Creates a new scenario builder
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Sets the broker configuration
    #[must_use]
    pub fn with_broker(mut self, config: TurmoilBrokerConfig) -> Self {
        self.broker_config = config;
        self
    }

    /// Adds a client to the scenario
    #[must_use]
    pub fn with_client(mut self, config: TurmoilClientConfig) -> Self {
        self.clients.push(config);
        self
    }

    /// Adds multiple clients with a naming pattern
    #[must_use]
    pub fn with_clients(mut self, count: usize, prefix: &str) -> Self {
        for i in 0..count {
            self.clients.push(TurmoilClientConfig {
                client_id: format!("{}-{}", prefix, i),
                ..Default::default()
            });
        }
        self
    }

    /// Sets network conditions between nodes
    #[must_use]
    pub fn with_network_conditions(
        mut self,
        from: &str,
        to: &str,
        conditions: NetworkConditions,
    ) -> Self {
        let key = format!("{}->{}", from, to);
        self.network_conditions.insert(key, conditions);
        self
    }

    /// Sets the scenario duration
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    /// Builds the scenario
    #[must_use]
    pub fn build(self) -> MqttScenario {
        MqttScenario {
            name: self.name,
            broker_config: self.broker_config,
            clients: self.clients,
            network_conditions: self.network_conditions,
            duration: self.duration,
        }
    }
}

/// Represents an MQTT test scenario
pub struct MqttScenario {
    pub name: String,
    pub broker_config: TurmoilBrokerConfig,
    pub clients: Vec<TurmoilClientConfig>,
    pub network_conditions: HashMap<String, NetworkConditions>,
    pub duration: Duration,
}

impl MqttScenario {
    /// Creates a publisher-subscriber scenario
    #[must_use]
    pub fn pub_sub(publishers: usize, subscribers: usize) -> Self {
        ScenarioBuilder::new("pub-sub")
            .with_clients(publishers, "pub")
            .with_clients(subscribers, "sub")
            .build()
    }

    /// Creates a fan-out scenario
    #[must_use]
    pub fn fan_out(publisher_count: usize, subscriber_count: usize) -> Self {
        ScenarioBuilder::new("fan-out")
            .with_clients(publisher_count, "publisher")
            .with_clients(subscriber_count, "subscriber")
            .build()
    }

    /// Creates a stress test scenario
    #[must_use]
    pub fn stress_test(client_count: usize) -> Self {
        ScenarioBuilder::new("stress-test")
            .with_clients(client_count, "client")
            .with_duration(Duration::from_secs(300))
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_conditions() {
        let ideal = NetworkConditions::ideal();
        assert_eq!(ideal.latency, Duration::from_millis(0));
        assert!((ideal.packet_loss - 0.0).abs() < f32::EPSILON);

        let wan = NetworkConditions::wan();
        assert_eq!(wan.latency, Duration::from_millis(50));
        assert!((wan.packet_loss - 0.01).abs() < f32::EPSILON);
    }

    #[test]
    fn test_scenario_builder() {
        let scenario = ScenarioBuilder::new("test")
            .with_clients(5, "client")
            .with_duration(Duration::from_secs(60))
            .build();

        assert_eq!(scenario.name, "test");
        assert_eq!(scenario.clients.len(), 5);
        assert_eq!(scenario.duration, Duration::from_secs(60));
    }

    #[test]
    fn test_predefined_scenarios() {
        let pub_sub = MqttScenario::pub_sub(2, 5);
        assert_eq!(pub_sub.clients.len(), 7);

        let stress = MqttScenario::stress_test(100);
        assert_eq!(stress.clients.len(), 100);
        assert_eq!(stress.duration, Duration::from_secs(300));
    }

    #[test]
    fn test_network_conditions_equality() {
        let conditions = NetworkConditions::lan();
        assert!((conditions.packet_loss - 0.0).abs() < f32::EPSILON);

        let poor = NetworkConditions::poor();
        assert!((poor.packet_loss - 0.05).abs() < f32::EPSILON);

        let mobile = NetworkConditions::mobile();
        assert!((mobile.packet_loss - 0.02).abs() < f32::EPSILON);

        let wan = NetworkConditions::wan();
        assert!((wan.packet_loss - 0.01).abs() < f32::EPSILON);
    }
}
