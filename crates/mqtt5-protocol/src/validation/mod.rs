use crate::error::{MqttError, Result};
use crate::prelude::{format, String, ToString, Vec};

pub mod namespace;
mod shared_subscription;

pub use shared_subscription::{parse_shared_subscription, strip_shared_subscription_prefix};

/// Validates an MQTT topic name according to MQTT v5.0 specification
///
/// # Rules:
/// - Must be UTF-8 encoded
/// - Must have at least one character
/// - Must not contain null characters (U+0000)
/// - Must not exceed maximum string length when UTF-8 encoded
/// - Should not contain wildcard characters (+, #) in topic names (only in filters)
#[must_use]
pub fn is_valid_topic_name(topic: &str) -> bool {
    if topic.is_empty() {
        return false;
    }

    if topic.len() > crate::constants::limits::MAX_STRING_LENGTH as usize {
        return false;
    }

    if topic.contains('\0') {
        return false;
    }

    if topic.contains('+') || topic.contains('#') {
        return false;
    }

    true
}

/// Validates an MQTT topic filter according to MQTT v5.0 specification
///
/// # Rules:
/// - Must follow all topic name rules except wildcard usage
/// - Single-level wildcard (+) must occupy entire level
/// - Multi-level wildcard (#) must be last character and occupy entire level
/// - Examples: sport/+/player, sport/tennis/#, +/tennis/#
#[must_use]
pub fn is_valid_topic_filter(filter: &str) -> bool {
    if filter.is_empty() {
        return false;
    }

    if filter.len() > crate::constants::limits::MAX_STRING_LENGTH as usize {
        return false;
    }

    if filter.contains('\0') {
        return false;
    }

    let parts: Vec<&str> = filter.split('/').collect();

    for (i, part) in parts.iter().enumerate() {
        if part.contains('#') {
            if i != parts.len() - 1 {
                return false;
            }
            if *part != "#" {
                return false;
            }
        }

        if part.contains('+') && *part != "+" {
            return false;
        }
    }

    true
}

#[must_use]
pub fn is_valid_subscription_filter(filter: &str) -> bool {
    let first_level = filter.split('/').next().unwrap_or(filter);
    if first_level != "$share" {
        return is_valid_topic_filter(filter);
    }
    if filter.len() > crate::constants::limits::MAX_STRING_LENGTH as usize {
        return false;
    }
    filter
        .strip_prefix("$share/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(share_name, topic_filter)| {
            !share_name.is_empty()
                && !share_name.contains(['+', '#'])
                && is_valid_topic_filter(topic_filter)
        })
}

/// # Errors
///
/// Returns `MqttError::InvalidTopicFilter` if [`is_valid_subscription_filter`] rejects it
pub fn validate_subscription_filter(filter: &str) -> Result<()> {
    if !is_valid_subscription_filter(filter) {
        return Err(MqttError::InvalidTopicFilter(filter.to_string()));
    }
    Ok(())
}

/// Validates an MQTT client identifier according to MQTT v5.0 specification
///
/// # Rules:
/// - Must be UTF-8 encoded
/// - Must contain only characters: 0-9, a-z, A-Z
/// - Must be between 1 and 23 bytes (unless server supports longer)
/// - Empty string is allowed (server will assign one)
#[must_use]
pub fn is_valid_client_id(client_id: &str) -> bool {
    if client_id.is_empty() {
        return true;
    }

    if client_id.len() > crate::constants::limits::MAX_CLIENT_ID_LENGTH {
        return false;
    }

    client_id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Checks whether a client ID is safe for use in filesystem paths.
///
/// Rejects path traversal sequences (`..`), path separators (`/`, `\`),
/// null bytes, and control characters while allowing common client ID
/// characters like hyphens, underscores, and dots (when not part of `..`).
#[must_use]
pub fn is_path_safe_client_id(client_id: &str) -> bool {
    if client_id.is_empty() {
        return true;
    }

    if client_id.len() > crate::constants::limits::MAX_CLIENT_ID_LENGTH {
        return false;
    }

    if client_id.contains('/') || client_id.contains('\\') || client_id.contains('\0') {
        return false;
    }

    if client_id == "." || client_id == ".." || client_id.starts_with("../") {
        return false;
    }

    client_id.chars().all(|c| !c.is_ascii_control())
}

/// Validates a topic name and returns an error if invalid
///
/// # Errors
///
/// Returns `MqttError::InvalidTopicName` if the topic name:
/// - Is empty
/// - Exceeds maximum string length
/// - Contains null characters
/// - Contains wildcard characters (+, #)
pub fn validate_topic_name(topic: &str) -> Result<()> {
    if !is_valid_topic_name(topic) {
        return Err(MqttError::InvalidTopicName(topic.to_string()));
    }
    Ok(())
}

/// Validates a topic filter and returns an error if invalid
///
/// # Errors
///
/// Returns `MqttError::InvalidTopicFilter` if the topic filter:
/// - Is empty
/// - Exceeds maximum string length
/// - Contains null characters
/// - Has invalid wildcard usage
pub fn validate_topic_filter(filter: &str) -> Result<()> {
    if !is_valid_topic_filter(filter) {
        return Err(MqttError::InvalidTopicFilter(filter.to_string()));
    }
    Ok(())
}

/// Validates a client ID and returns an error if invalid
///
/// # Errors
///
/// Returns `MqttError::InvalidClientId` if the client ID:
/// - Contains non-alphanumeric characters
/// - Exceeds maximum client ID length
pub fn validate_client_id(client_id: &str) -> Result<()> {
    if !is_valid_client_id(client_id) {
        return Err(MqttError::InvalidClientId(client_id.to_string()));
    }
    Ok(())
}

/// Checks if a topic name matches a topic filter
///
/// # Rules:
/// - '+' matches exactly one topic level
/// - '#' matches any number of levels including parent level
/// - Topic and filter level separators must match exactly
/// - Topics starting with '$' do NOT match root-level wildcards (MQTT spec)
#[must_use]
pub fn topic_matches_filter(topic: &str, filter: &str) -> bool {
    if topic.starts_with('$') && (filter.starts_with('#') || filter.starts_with('+')) {
        return false;
    }

    if filter == "#" {
        return true;
    }

    let topic_parts: Vec<&str> = topic.split('/').collect();
    let filter_parts: Vec<&str> = filter.split('/').collect();

    let mut t_idx = 0;
    let mut f_idx = 0;

    while t_idx < topic_parts.len() && f_idx < filter_parts.len() {
        if filter_parts[f_idx] == "#" {
            return true;
        }

        if filter_parts[f_idx] != "+" && filter_parts[f_idx] != topic_parts[t_idx] {
            return false;
        }

        t_idx += 1;
        f_idx += 1;
    }

    if t_idx == topic_parts.len() && f_idx == filter_parts.len() {
        return true;
    }

    if t_idx == topic_parts.len() && f_idx == filter_parts.len() - 1 && filter_parts[f_idx] == "#" {
        return true;
    }

    false
}

/// Trait for pluggable topic validation
///
/// This trait allows customization of topic validation rules beyond the standard MQTT specification.
/// Implementations can add additional restrictions, reserved topic prefixes, or cloud provider-specific rules.
pub trait TopicValidator: Send + Sync {
    /// Validates a topic name for publishing
    ///
    /// # Arguments
    ///
    /// * `topic` - The topic name to validate
    ///
    /// # Errors
    ///
    /// Returns `MqttError::InvalidTopicName` if the topic is invalid
    fn validate_topic_name(&self, topic: &str) -> Result<()>;

    /// Validates a topic filter for subscriptions
    ///
    /// # Arguments
    ///
    /// * `filter` - The topic filter to validate
    ///
    /// # Errors
    ///
    /// Returns `MqttError::InvalidTopicFilter` if the filter is invalid
    fn validate_topic_filter(&self, filter: &str) -> Result<()>;

    /// Checks if a topic is reserved and should be restricted
    ///
    /// # Arguments
    ///
    /// * `topic` - The topic to check
    ///
    /// # Returns
    ///
    /// `true` if the topic is reserved and should be restricted
    fn is_reserved_topic(&self, topic: &str) -> bool;

    /// Gets a human-readable description of the validator
    fn description(&self) -> &'static str;
}

/// Standard MQTT specification validator
///
/// This validator implements the basic MQTT v5.0 specification rules for topic names and filters.
#[derive(Debug, Clone, Default)]
pub struct StandardValidator;

impl TopicValidator for StandardValidator {
    fn validate_topic_name(&self, topic: &str) -> Result<()> {
        validate_topic_name(topic)
    }

    fn validate_topic_filter(&self, filter: &str) -> Result<()> {
        validate_topic_filter(filter)
    }

    fn is_reserved_topic(&self, _topic: &str) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Standard MQTT v5.0 specification validator"
    }
}

/// Restrictive validator with additional constraints
///
/// This validator extends the standard MQTT rules with additional restrictions
/// such as reserved topic prefixes, maximum topic levels, and custom character sets.
#[derive(Debug, Clone, Default)]
pub struct RestrictiveValidator {
    /// Reserved topic prefixes that should be rejected
    pub reserved_prefixes: Vec<String>,
    /// Maximum number of topic levels (separated by '/')
    pub max_levels: Option<usize>,
    /// Maximum topic length (overrides MQTT spec if smaller)
    pub max_topic_length: Option<usize>,
    /// Prohibited characters beyond MQTT spec requirements
    pub prohibited_chars: Vec<char>,
}

impl RestrictiveValidator {
    /// Creates a new restrictive validator
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a reserved topic prefix
    #[must_use]
    pub fn with_reserved_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.reserved_prefixes.push(prefix.into());
        self
    }

    /// Sets the maximum number of topic levels
    #[must_use]
    pub fn with_max_levels(mut self, max_levels: usize) -> Self {
        self.max_levels = Some(max_levels);
        self
    }

    /// Sets the maximum topic length
    #[must_use]
    pub fn with_max_topic_length(mut self, max_length: usize) -> Self {
        self.max_topic_length = Some(max_length);
        self
    }

    /// Adds a prohibited character
    #[must_use]
    pub fn with_prohibited_char(mut self, ch: char) -> Self {
        self.prohibited_chars.push(ch);
        self
    }

    /// Checks if topic violates additional restrictions
    fn check_additional_restrictions(&self, topic: &str) -> Result<()> {
        for prefix in &self.reserved_prefixes {
            if topic.starts_with(prefix) {
                return Err(MqttError::InvalidTopicName(format!(
                    "Topic '{topic}' uses reserved prefix '{prefix}'"
                )));
            }
        }

        if let Some(max_levels) = self.max_levels {
            let level_count = topic.split('/').count();
            if level_count > max_levels {
                return Err(MqttError::InvalidTopicName(format!(
                    "Topic '{topic}' has {level_count} levels, maximum allowed is {max_levels}"
                )));
            }
        }

        if let Some(max_length) = self.max_topic_length {
            if topic.len() > max_length {
                return Err(MqttError::InvalidTopicName(format!(
                    "Topic '{}' length {} exceeds maximum {}",
                    topic,
                    topic.len(),
                    max_length
                )));
            }
        }

        for &prohibited_char in &self.prohibited_chars {
            if topic.contains(prohibited_char) {
                return Err(MqttError::InvalidTopicName(format!(
                    "Topic '{topic}' contains prohibited character '{prohibited_char}'"
                )));
            }
        }

        Ok(())
    }
}

impl TopicValidator for RestrictiveValidator {
    fn validate_topic_name(&self, topic: &str) -> Result<()> {
        validate_topic_name(topic)?;
        self.check_additional_restrictions(topic)
    }

    fn validate_topic_filter(&self, filter: &str) -> Result<()> {
        validate_topic_filter(filter)?;

        for prefix in &self.reserved_prefixes {
            if filter.starts_with(prefix) && !filter.contains('+') && !filter.contains('#') {
                return Err(MqttError::InvalidTopicFilter(format!(
                    "Topic filter '{filter}' uses reserved prefix '{prefix}'"
                )));
            }
        }

        Ok(())
    }

    fn is_reserved_topic(&self, topic: &str) -> bool {
        self.reserved_prefixes
            .iter()
            .any(|prefix| topic.starts_with(prefix))
    }

    fn description(&self) -> &'static str {
        "Restrictive validator with additional constraints"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_topic_names() {
        assert!(is_valid_topic_name("sport/tennis"));
        assert!(is_valid_topic_name("sport/tennis/player1"));
        assert!(is_valid_topic_name("home/temperature"));
        assert!(is_valid_topic_name("/"));
        assert!(is_valid_topic_name("a"));
    }

    #[test]
    fn test_invalid_topic_names() {
        assert!(!is_valid_topic_name(""));
        assert!(!is_valid_topic_name("sport/+/player"));
        assert!(!is_valid_topic_name("sport/tennis/#"));
        assert!(!is_valid_topic_name("home\0temperature"));

        let too_long = "a".repeat(crate::constants::limits::MAX_BINARY_LENGTH as usize + 1);
        assert!(!is_valid_topic_name(&too_long));
    }

    #[test]
    fn test_valid_topic_filters() {
        assert!(is_valid_topic_filter("sport/tennis"));
        assert!(is_valid_topic_filter("sport/+/player"));
        assert!(is_valid_topic_filter("sport/tennis/#"));
        assert!(is_valid_topic_filter("#"));
        assert!(is_valid_topic_filter("+"));
        assert!(is_valid_topic_filter("+/tennis/#"));
        assert!(is_valid_topic_filter("sport/+"));
    }

    #[test]
    fn test_invalid_topic_filters() {
        assert!(!is_valid_topic_filter(""));
        assert!(!is_valid_topic_filter("sport/tennis#"));
        assert!(!is_valid_topic_filter("sport/tennis/#/ranking"));
        assert!(!is_valid_topic_filter("sport+"));
        assert!(!is_valid_topic_filter("sport/+tennis"));
        assert!(!is_valid_topic_filter("home\0temperature"));
    }

    #[test]
    fn test_valid_subscription_filters() {
        for filter in [
            "$shared/x",
            "$sharex",
            "$share/g/a/+",
            "$share/g/#",
            "$share/g//",
            "/",
            "+/+",
            "a/+/b/#",
            "+",
            "#",
            "$SYS/#",
            "a//b",
        ] {
            assert!(is_valid_subscription_filter(filter), "{filter}");
        }
    }

    #[test]
    fn test_invalid_subscription_filters() {
        for filter in [
            "",
            "a/#/b",
            "a+",
            "$share",
            "$share/",
            "$share//x",
            "$share/g",
            "$share/g/",
            "$share/g/a/#/b",
            "$share/g+/x",
            "$share/g#/x",
            "$share/+/x",
            "$share/#/x",
            "$share/g/a\0",
        ] {
            assert!(!is_valid_subscription_filter(filter), "{filter:?}");
            assert!(validate_subscription_filter(filter).is_err());
        }
    }

    #[test]
    fn test_valid_client_ids() {
        assert!(is_valid_client_id(""));
        assert!(is_valid_client_id("client123"));
        assert!(is_valid_client_id("MyClient"));
        assert!(is_valid_client_id("123456789012345678901234"));
        assert!(is_valid_client_id("a1b2c3d4e5f6"));
    }

    #[test]
    fn test_invalid_client_ids() {
        assert!(!is_valid_client_id("client-123"));
        assert!(!is_valid_client_id("client.123"));
        assert!(!is_valid_client_id("client 123"));
        assert!(!is_valid_client_id("client@123"));

        let too_long = "a".repeat(crate::constants::limits::MAX_CLIENT_ID_LENGTH + 1);
        assert!(!is_valid_client_id(&too_long));
    }

    #[test]
    fn test_path_safe_client_ids_valid() {
        assert!(is_path_safe_client_id(""));
        assert!(is_path_safe_client_id("client123"));
        assert!(is_path_safe_client_id("my-device-001"));
        assert!(is_path_safe_client_id("sensor_node.5"));
        assert!(is_path_safe_client_id("client@home"));
        assert!(is_path_safe_client_id("device 1"));
    }

    #[test]
    fn test_path_safe_client_ids_rejects_traversal() {
        assert!(!is_path_safe_client_id("."));
        assert!(!is_path_safe_client_id(".."));
        assert!(!is_path_safe_client_id("../etc"));
        assert!(!is_path_safe_client_id("foo/../../etc"));
        assert!(!is_path_safe_client_id("a/../b"));
        assert!(!is_path_safe_client_id("foo/bar"));
        assert!(!is_path_safe_client_id("foo\\bar"));
        assert!(!is_path_safe_client_id("/etc/passwd"));
        assert!(!is_path_safe_client_id("client\0id"));
        assert!(!is_path_safe_client_id("client\x01id"));

        let too_long = "a".repeat(crate::constants::limits::MAX_CLIENT_ID_LENGTH + 1);
        assert!(!is_path_safe_client_id(&too_long));
    }

    #[test]
    fn test_topic_matches_filter() {
        assert!(topic_matches_filter("sport/tennis", "sport/tennis"));

        assert!(topic_matches_filter("sport/tennis", "sport/+"));
        assert!(topic_matches_filter(
            "sport/tennis/player1",
            "sport/+/player1"
        ));
        assert!(topic_matches_filter(
            "sport/tennis/player1",
            "sport/tennis/+"
        ));
        assert!(!topic_matches_filter("sport/tennis/player1", "sport/+"));

        assert!(topic_matches_filter("sport/tennis", "sport/#"));
        assert!(topic_matches_filter("sport/tennis/player1", "sport/#"));
        assert!(topic_matches_filter(
            "sport/tennis/player1/ranking",
            "sport/#"
        ));
        assert!(topic_matches_filter("sport", "sport/#"));
        assert!(topic_matches_filter("anything", "#"));
        assert!(topic_matches_filter("sport/tennis", "#"));

        assert!(!topic_matches_filter("$SYS/broker/uptime", "#"));
        assert!(!topic_matches_filter(
            "$SYS/broker/uptime",
            "+/broker/uptime"
        ));
        assert!(!topic_matches_filter("$data/temp", "+/temp"));
        assert!(topic_matches_filter("$SYS/broker/uptime", "$SYS/#"));
        assert!(topic_matches_filter("$SYS/broker/uptime", "$SYS/+/uptime"));

        assert!(!topic_matches_filter("sport/tennis", "sport/football"));
        assert!(!topic_matches_filter("sport", "sport/tennis"));
        assert!(!topic_matches_filter(
            "sport/tennis/player1",
            "sport/tennis"
        ));
    }
}
