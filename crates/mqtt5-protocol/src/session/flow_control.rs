use crate::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct FlowControlConfig {
    pub enable_backpressure: bool,
    pub backpressure_timeout: Option<Duration>,
    pub max_pending_queue_size: usize,
    pub in_flight_timeout: Duration,
}

impl Default for FlowControlConfig {
    fn default() -> Self {
        Self {
            enable_backpressure: true,
            backpressure_timeout: Some(Duration::from_secs(30)),
            max_pending_queue_size: 1000,
            in_flight_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FlowControlStats {
    pub receive_maximum: u16,
    pub in_flight_count: usize,
    pub available_quota: usize,
    pub pending_requests: usize,
    pub oldest_in_flight: Option<Instant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flow_control_config_default() {
        let config = FlowControlConfig::default();
        assert!(config.enable_backpressure);
        assert_eq!(config.backpressure_timeout, Some(Duration::from_secs(30)));
        assert_eq!(config.max_pending_queue_size, 1000);
        assert_eq!(config.in_flight_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_flow_control_stats() {
        let stats = FlowControlStats {
            receive_maximum: 65535,
            in_flight_count: 10,
            available_quota: 100,
            pending_requests: 5,
            oldest_in_flight: Some(Instant::now()),
        };

        assert_eq!(stats.receive_maximum, 65535);
        assert_eq!(stats.in_flight_count, 10);
        assert_eq!(stats.available_quota, 100);
        assert_eq!(stats.pending_requests, 5);
        assert!(stats.oldest_in_flight.is_some());
    }
}
