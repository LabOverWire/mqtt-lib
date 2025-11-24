use crate::error::{MqttError, Result};
use crate::validation::{validate_topic_filter, validate_topic_name, TopicValidator};

/// Namespace-based topic validator for hierarchical topic isolation
///
/// This validator implements namespace-based topic validation patterns that enforce
/// hierarchical isolation of topics, commonly used in cloud `IoT` platforms and
/// enterprise systems.
///
/// The validator enforces:
/// - Service-reserved topic prefixes (e.g., `$aws/`, `$azure/`, `$company/`)
/// - Device-specific namespaces (e.g., `{prefix}/thing/{device}/`, `{prefix}/device/{device}/`)
/// - System topics (e.g., `$SYS/`)
///
/// Examples:
/// - AWS `IoT`: `NamespaceValidator::new("$aws", "thing")`
/// - Azure `IoT`: `NamespaceValidator::new("$azure", "device")`
/// - Enterprise: `NamespaceValidator::new("$company", "asset")`
#[derive(Debug, Clone)]
pub struct NamespaceValidator {
    /// Service prefix for reserved topics (e.g., "$aws", "$azure", "$company")
    pub service_prefix: String,
    /// Device namespace identifier (e.g., "thing", "device", "asset")
    pub device_namespace: String,
    /// Optional device identifier for device-specific validation
    /// If set, allows `{service_prefix}/{device_namespace}/{device_id}/*` topics
    pub device_id: Option<String>,
    /// Whether to allow system topics (`$SYS/*`)
    pub allow_system_topics: bool,
    /// Additional reserved prefixes beyond the service prefix
    pub additional_reserved_prefixes: Vec<String>,
}

impl NamespaceValidator {
    /// Creates a new namespace validator
    ///
    /// # Arguments
    /// * `service_prefix` - The service prefix (e.g., "$aws", "$azure")
    /// * `device_namespace` - The device namespace pattern (e.g., "thing", "device")
    #[must_use]
    pub fn new(service_prefix: impl Into<String>, device_namespace: impl Into<String>) -> Self {
        Self {
            service_prefix: service_prefix.into(),
            device_namespace: device_namespace.into(),
            device_id: None,
            allow_system_topics: false,
            additional_reserved_prefixes: Vec::new(),
        }
    }

    /// Creates a validator configured for AWS `IoT` Core
    #[must_use]
    pub fn aws_iot() -> Self {
        Self::new("$aws", "things")
    }

    /// Creates a validator configured for Azure `IoT` Hub
    #[must_use]
    pub fn azure_iot() -> Self {
        Self::new("$azure", "device")
    }

    /// Creates a validator configured for Google Cloud `IoT`
    #[must_use]
    pub fn google_cloud_iot() -> Self {
        Self::new("$gcp", "device")
    }

    /// Sets the device identifier for device-specific topic validation
    ///
    /// When set, allows topics like `{prefix}/{namespace}/{device_id}/*` while
    /// rejecting `{prefix}/{namespace}/{other_device}/*`
    #[must_use]
    pub fn with_device_id(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    /// Enables system topics (`$SYS/*`)
    #[must_use]
    pub fn with_system_topics(mut self, allow: bool) -> Self {
        self.allow_system_topics = allow;
        self
    }

    /// Adds an additional reserved prefix
    #[must_use]
    pub fn with_reserved_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.additional_reserved_prefixes.push(prefix.into());
        self
    }

    /// Checks if a topic is a service topic (e.g., `$aws/*`, `$azure/*`)
    fn is_service_topic(&self, topic: &str) -> bool {
        topic.starts_with(&format!("{}/", self.service_prefix))
    }

    /// Checks if a topic is a system topic (`$SYS/*`)  
    fn is_system_topic(topic: &str) -> bool {
        topic.starts_with("$SYS/")
    }

    /// Validates namespace-based topic restrictions
    fn validate_namespace_restrictions(&self, topic: &str) -> Result<()> {
        // Check system topics
        if Self::is_system_topic(topic) && !self.allow_system_topics {
            return Err(MqttError::InvalidTopicName(
                "System topics ($SYS/*) are not allowed".to_string(),
            ));
        }

        // Check service topics
        if self.is_service_topic(topic) {
            // For AWS IoT, most service topics are read-only and shouldn't be published to
            if self.service_prefix == "$aws" {
                // AWS IoT reserved topics that clients should not publish to
                let aws_reserved_prefixes = ["$aws/certificates/", "$aws/provisioning-templates/"];

                // Check if it's a reserved AWS topic
                for reserved in &aws_reserved_prefixes {
                    if topic.starts_with(reserved) {
                        return Err(MqttError::InvalidTopicName(format!(
                            "Cannot publish to reserved AWS IoT topic: {topic}"
                        )));
                    }
                }

                // Check device-specific topics for AWS
                if topic.starts_with("$aws/things/") {
                    // AWS IoT topics like $aws/things/{thing}/shadow/get/accepted are read-only
                    // Only allow certain operations for publishing
                    if let Some(ref device_id) = self.device_id {
                        // Allow shadow update/delete and job operations for the configured device
                        let allowed_patterns = [
                            format!("$aws/things/{device_id}/shadow/update"),
                            format!("$aws/things/{device_id}/shadow/delete"),
                            format!("$aws/things/{device_id}/jobs/"),
                        ];
                        if !allowed_patterns
                            .iter()
                            .any(|pattern| topic.starts_with(pattern))
                        {
                            return Err(MqttError::InvalidTopicName(format!(
                                "Cannot publish to reserved AWS IoT topic: {topic}"
                            )));
                        }
                    } else {
                        return Err(MqttError::InvalidTopicName(
                            "Device-specific topics require device ID to be configured".to_string(),
                        ));
                    }
                }
            } else {
                // Original logic for non-AWS providers
                // Build the device namespace prefix (e.g., "$aws/thing/", "$azure/device/")
                let device_namespace_prefix =
                    format!("{}/{}/", self.service_prefix, self.device_namespace);

                // If we have a device ID, check device-specific topics
                if let Some(ref device_id) = self.device_id {
                    let device_prefix = format!("{device_namespace_prefix}{device_id}/");

                    // Allow device-specific topics
                    if topic.starts_with(&device_prefix) {
                        return Ok(());
                    }

                    // Allow general service topics that don't start with device namespace
                    if !topic.starts_with(&device_namespace_prefix) {
                        // These are general service topics like $aws/events, $azure/operations, etc.
                        // Allow them for now, but this could be further restricted based on requirements
                        return Ok(());
                    }

                    // Reject topics for other devices
                    if topic.starts_with(&device_namespace_prefix) {
                        return Err(MqttError::InvalidTopicName(format!(
                            "Topic '{topic}' is for a different device. Only topics under '{device_prefix}' are allowed"
                        )));
                    }
                } else {
                    // No device ID set - reject all device-specific topics
                    if topic.starts_with(&device_namespace_prefix) {
                        return Err(MqttError::InvalidTopicName(format!(
                            "Device-specific topics ({device_namespace_prefix}*) require device ID to be configured"
                        )));
                    }
                    // Allow other service topics
                }
            }
        }

        // Check additional reserved prefixes
        for prefix in &self.additional_reserved_prefixes {
            if topic.starts_with(prefix) {
                return Err(MqttError::InvalidTopicName(format!(
                    "Topic '{topic}' uses reserved prefix '{prefix}'"
                )));
            }
        }

        Ok(())
    }
}

impl TopicValidator for NamespaceValidator {
    fn validate_topic_name(&self, topic: &str) -> Result<()> {
        // First apply standard MQTT validation
        validate_topic_name(topic)?;

        // AWS IoT has a 256 character limit
        if self.service_prefix == "$aws" && topic.len() > 256 {
            return Err(MqttError::InvalidTopicName(
                "AWS IoT topics must not exceed 256 characters".to_string(),
            ));
        }

        // Then apply namespace-specific restrictions
        self.validate_namespace_restrictions(topic)
    }

    fn validate_topic_filter(&self, filter: &str) -> Result<()> {
        // First apply standard MQTT validation
        validate_topic_filter(filter)?;

        // AWS IoT has a 256 character limit
        if self.service_prefix == "$aws" && filter.len() > 256 {
            return Err(MqttError::InvalidTopicFilter(
                "AWS IoT topic filters must not exceed 256 characters".to_string(),
            ));
        }

        // For filters, we're more lenient with wildcards
        // Only check if it's a literal reserved prefix (no wildcards)
        if !filter.contains('+') && !filter.contains('#') {
            self.validate_namespace_restrictions(filter)?;
        }

        Ok(())
    }

    fn is_reserved_topic(&self, topic: &str) -> bool {
        // Check service topics
        if self.is_service_topic(topic) {
            let device_namespace_prefix =
                format!("{}/{}/", self.service_prefix, self.device_namespace);

            // If we have a device ID, only topics outside our device are reserved
            if let Some(ref device_id) = self.device_id {
                let device_prefix = format!("{device_namespace_prefix}{device_id}/");
                return !topic.starts_with(&device_prefix)
                    && topic.starts_with(&device_namespace_prefix);
            }
            // If no device ID, all device namespace topics are reserved
            return topic.starts_with(&device_namespace_prefix);
        }

        // Check system topics
        if Self::is_system_topic(topic) && !self.allow_system_topics {
            return true;
        }

        // Check additional reserved prefixes
        self.additional_reserved_prefixes
            .iter()
            .any(|prefix| topic.starts_with(prefix))
    }

    fn description(&self) -> &'static str {
        "Namespace-based topic validator with hierarchical isolation"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespace_validator_basic() {
        let validator = NamespaceValidator::new("$aws", "thing");

        // Regular topics should work
        assert!(validator.validate_topic_name("sensor/temperature").is_ok());
        assert!(validator.validate_topic_filter("sensor/+").is_ok());

        // System topics should be rejected by default
        assert!(validator
            .validate_topic_name("$SYS/broker/version")
            .is_err());
        assert!(!validator.is_reserved_topic("regular/topic"));
        assert!(validator.is_reserved_topic("$SYS/broker/version"));
    }

    #[test]
    fn test_namespace_validator_with_device() {
        let validator = NamespaceValidator::new("$aws", "things").with_device_id("my-device");

        // Device-specific topics should work
        assert!(validator
            .validate_topic_name("$aws/things/my-device/shadow/update")
            .is_ok());

        // Other device topics should be rejected
        assert!(validator
            .validate_topic_name("$aws/things/other-device/shadow/update")
            .is_err());

        // General AWS topics should work
        assert!(validator
            .validate_topic_name("$aws/events/presence/connected/my-device")
            .is_ok());
    }

    #[test]
    fn test_namespace_validator_system_topics() {
        let validator = NamespaceValidator::new("$aws", "thing").with_system_topics(true);

        // System topics should now work
        assert!(validator.validate_topic_name("$SYS/broker/version").is_ok());
        assert!(!validator.is_reserved_topic("$SYS/broker/version"));
    }

    #[test]
    fn test_namespace_validator_additional_prefixes() {
        let validator = NamespaceValidator::new("$aws", "thing")
            .with_reserved_prefix("company/")
            .with_reserved_prefix("internal/");

        // Additional reserved prefixes should be rejected
        assert!(validator.validate_topic_name("company/secret").is_err());
        assert!(validator.validate_topic_name("internal/admin").is_err());
        assert!(validator.is_reserved_topic("company/secret"));

        // Regular topics should work
        assert!(validator.validate_topic_name("public/sensor").is_ok());
    }

    #[test]
    fn test_different_cloud_providers() {
        // Test AWS IoT
        let aws = NamespaceValidator::aws_iot().with_device_id("sensor-123");
        assert!(aws
            .validate_topic_name("$aws/things/sensor-123/shadow/update")
            .is_ok());
        assert!(aws
            .validate_topic_name("$aws/things/sensor-456/shadow/update")
            .is_err());

        // Test Azure IoT
        let azure = NamespaceValidator::azure_iot().with_device_id("device-abc");
        assert!(azure
            .validate_topic_name("$azure/device/device-abc/telemetry")
            .is_ok());
        assert!(azure
            .validate_topic_name("$azure/device/device-xyz/telemetry")
            .is_err());

        // Test custom enterprise
        let enterprise = NamespaceValidator::new("$company", "asset").with_device_id("machine-001");
        assert!(enterprise
            .validate_topic_name("$company/asset/machine-001/status")
            .is_ok());
        assert!(enterprise
            .validate_topic_name("$company/asset/machine-002/status")
            .is_err());
    }
}
