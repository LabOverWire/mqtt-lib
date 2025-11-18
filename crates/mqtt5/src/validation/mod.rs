use crate::error::{MqttError, Result};

pub mod namespace;

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

    // Topic names should not contain wildcards
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
        // Multi-level wildcard rules
        if part.contains('#') {
            // # must be the last character in the filter
            if i != parts.len() - 1 {
                return false;
            }
            // # must occupy the entire level
            if *part != "#" {
                return false;
            }
        }

        // Single-level wildcard rules
        if part.contains('+') {
            // + must occupy the entire level
            if *part != "+" {
                return false;
            }
        }
    }

    true
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
        return true; // Empty client ID is allowed
    }

    if client_id.len() > 23 {
        // Most servers support longer, but 23 is the spec minimum
        // We'll allow longer and let the server reject if needed
        if client_id.len() > crate::constants::limits::MAX_CLIENT_ID_LENGTH {
            return false; // Reasonable upper limit
        }
    }

    // Check for valid characters (alphanumeric)
    client_id.chars().all(|c| c.is_ascii_alphanumeric())
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
    // MQTT spec: topics starting with $ do not match wildcards at root level
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
            return true; // Multi-level wildcard matches everything remaining
        }

        if filter_parts[f_idx] != "+" && filter_parts[f_idx] != topic_parts[t_idx] {
            return false; // Not a match
        }

        t_idx += 1;
        f_idx += 1;
    }

    // Check if we've consumed both topic and filter
    if t_idx == topic_parts.len() && f_idx == filter_parts.len() {
        return true;
    }

    // Check if filter ends with # and we've consumed the topic
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
        // Standard MQTT has no reserved topics
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
        // Check reserved prefixes
        for prefix in &self.reserved_prefixes {
            if topic.starts_with(prefix) {
                return Err(MqttError::InvalidTopicName(format!(
                    "Topic '{topic}' uses reserved prefix '{prefix}'"
                )));
            }
        }

        // Check maximum levels
        if let Some(max_levels) = self.max_levels {
            let level_count = topic.split('/').count();
            if level_count > max_levels {
                return Err(MqttError::InvalidTopicName(format!(
                    "Topic '{topic}' has {level_count} levels, maximum allowed is {max_levels}"
                )));
            }
        }

        // Check maximum length
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

        // Check prohibited characters
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
        // First apply standard validation
        validate_topic_name(topic)?;
        // Then apply additional restrictions
        self.check_additional_restrictions(topic)
    }

    fn validate_topic_filter(&self, filter: &str) -> Result<()> {
        // First apply standard validation
        validate_topic_filter(filter)?;
        // Then apply additional restrictions (but allow wildcards)
        // Note: We don't apply all restrictions to filters since they may contain wildcards

        // Check reserved prefixes
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

        let too_long = "a".repeat(crate::constants::limits::MAX_BINARY_LENGTH as usize);
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
    fn test_topic_matches_filter() {
        // Exact matches
        assert!(topic_matches_filter("sport/tennis", "sport/tennis"));

        // Single-level wildcard
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

        // Multi-level wildcard
        assert!(topic_matches_filter("sport/tennis", "sport/#"));
        assert!(topic_matches_filter("sport/tennis/player1", "sport/#"));
        assert!(topic_matches_filter(
            "sport/tennis/player1/ranking",
            "sport/#"
        ));
        assert!(topic_matches_filter("sport", "sport/#"));
        assert!(topic_matches_filter("anything", "#"));
        assert!(topic_matches_filter("sport/tennis", "#"));

        // $ prefix topics - MQTT spec compliant behavior
        assert!(!topic_matches_filter("$SYS/broker/uptime", "#"));
        assert!(!topic_matches_filter(
            "$SYS/broker/uptime",
            "+/broker/uptime"
        ));
        assert!(!topic_matches_filter("$data/temp", "+/temp"));
        assert!(topic_matches_filter("$SYS/broker/uptime", "$SYS/#"));
        assert!(topic_matches_filter("$SYS/broker/uptime", "$SYS/+/uptime"));

        // Non-matches
        assert!(!topic_matches_filter("sport/tennis", "sport/football"));
        assert!(!topic_matches_filter("sport", "sport/tennis"));
        assert!(!topic_matches_filter(
            "sport/tennis/player1",
            "sport/tennis"
        ));
    }
}
