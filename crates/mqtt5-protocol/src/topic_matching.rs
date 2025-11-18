/// Comprehensive topic matching implementation for MQTT
/// This module provides the core topic matching algorithm with full support
/// for single-level (+) and multi-level (#) wildcards according to MQTT spec
use crate::error::{MqttError, Result};

/// Matches a topic name against a topic filter with wildcard support
///
/// # Arguments
/// * `topic` - The topic name to match (no wildcards allowed)
/// * `filter` - The topic filter which may contain wildcards
///
/// # Returns
/// * `true` if the topic matches the filter
/// * `false` otherwise
///
/// # Examples
/// ```
/// # use mqtt5::topic_matching::matches;
/// assert!(matches("sport/tennis", "sport/tennis"));
/// assert!(matches("sport/tennis", "sport/+"));
/// assert!(matches("sport/tennis/player1", "sport/#"));
/// assert!(!matches("sport/tennis", "sport/+/player1"));
/// ```
#[must_use]
pub fn matches(topic: &str, filter: &str) -> bool {
    // Empty topic doesn't match anything
    if topic.is_empty() {
        return false;
    }

    // Validate inputs
    if !is_valid_topic(topic) || !is_valid_filter(filter) {
        return false;
    }

    // Fast path for exact match
    if topic == filter {
        return true;
    }

    // MQTT spec: topics starting with $ do not match wildcards at root level
    if topic.starts_with('$') && (filter.starts_with('#') || filter.starts_with('+')) {
        return false;
    }

    // Fast path for # at root
    if filter == "#" {
        return true;
    }

    let topic_parts: Vec<&str> = topic.split('/').collect();
    let filter_parts: Vec<&str> = filter.split('/').collect();

    match_parts(&topic_parts, &filter_parts)
}

/// Recursive helper for matching topic parts against filter parts
fn match_parts(topic_parts: &[&str], filter_parts: &[&str]) -> bool {
    match (topic_parts.first(), filter_parts.first()) {
        // Both exhausted - match
        (None, None) => true,

        // Filter has # - matches everything remaining
        (_, Some(&"#")) => filter_parts.len() == 1, // # must be last

        // One exhausted but not both - no match
        (None, Some(_)) | (Some(_), None) => false,

        // Both have parts
        (Some(&topic_part), Some(&filter_part)) => {
            // Check current level match
            let level_match = filter_part == "+" || filter_part == topic_part;

            // If current level matches, check remaining parts
            level_match && match_parts(&topic_parts[1..], &filter_parts[1..])
        }
    }
}

/// Validates a topic name (no wildcards allowed)
#[must_use]
pub fn is_valid_topic(topic: &str) -> bool {
    // Empty topic is actually valid in MQTT (e.g., for will messages)
    !topic.contains('\0') && !topic.contains('+') && !topic.contains('#') && topic.len() <= 65535
}

/// Validates a topic filter (may contain wildcards)
#[must_use]
pub fn is_valid_filter(filter: &str) -> bool {
    if filter.is_empty() || filter.contains('\0') || filter.len() > 65535 {
        return false;
    }

    let parts: Vec<&str> = filter.split('/').collect();

    for (i, part) in parts.iter().enumerate() {
        // # must be alone and last
        if part.contains('#') {
            return *part == "#" && i == parts.len() - 1;
        }

        // + must be alone in its level
        if part.contains('+') && *part != "+" {
            return false;
        }
    }

    true
}

/// Validates a topic and returns an error if invalid
///
/// # Errors
/// Returns `MqttError::InvalidTopicName` if the topic is invalid
pub fn validate_topic(topic: &str) -> Result<()> {
    if !is_valid_topic(topic) {
        return Err(MqttError::InvalidTopicName(format!(
            "Invalid topic: {}",
            if topic.is_empty() {
                "empty topic"
            } else if topic.contains('+') || topic.contains('#') {
                "wildcards not allowed in topic names"
            } else if topic.contains('\0') {
                "null character not allowed"
            } else if topic.len() > 65535 {
                "topic too long"
            } else {
                "unknown error"
            }
        )));
    }
    Ok(())
}

/// Validates a topic filter and returns an error if invalid
///
/// # Errors
/// Returns `MqttError::InvalidTopicFilter` if the filter is invalid
pub fn validate_filter(filter: &str) -> Result<()> {
    if !is_valid_filter(filter) {
        return Err(MqttError::InvalidTopicFilter(format!(
            "Invalid filter: {}",
            if filter.is_empty() {
                "empty filter"
            } else if filter.contains('\0') {
                "null character not allowed"
            } else if filter.len() > 65535 {
                "filter too long"
            } else {
                "invalid wildcard usage"
            }
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        assert!(matches("sport/tennis", "sport/tennis"));
        assert!(matches("/", "/"));
        assert!(matches("sport", "sport"));
        assert!(!matches("sport", "sports"));
        assert!(!matches("sport/tennis", "sport/tennis/player1"));
    }

    #[test]
    fn test_single_level_wildcard() {
        // Basic + usage
        assert!(matches("sport/tennis", "sport/+"));
        assert!(matches("sport/", "sport/+"));
        assert!(!matches("sport/tennis/player1", "sport/+"));

        // Multiple + in filter
        assert!(matches("sport/tennis/player1", "sport/+/+"));
        assert!(matches("sport/tennis/player1", "+/+/+"));
        assert!(!matches("sport/tennis", "+/+/+"));

        // + at different positions
        assert!(matches("sport/tennis", "+/tennis"));
        assert!(matches("sport/tennis/player1", "sport/tennis/+"));
        assert!(matches("/tennis", "+/tennis"));
        assert!(matches("sport/", "sport/+"));
    }

    #[test]
    fn test_multi_level_wildcard() {
        // # at end
        assert!(matches("sport", "sport/#"));
        assert!(matches("sport/", "sport/#"));
        assert!(matches("sport/tennis", "sport/#"));
        assert!(matches("sport/tennis/player1", "sport/#"));
        assert!(matches("sport/tennis/player1/ranking", "sport/#"));

        // # at root
        assert!(matches("sport", "#"));
        assert!(matches("sport/tennis", "#"));
        assert!(!matches("", "#")); // Empty topic never matches
        assert!(matches("/", "#"));

        // # not matching parent
        assert!(!matches("sports", "sport/#"));
        assert!(!matches("", "sport/#"));
    }

    #[test]
    fn test_mixed_wildcards() {
        assert!(matches("sport/tennis/player1", "sport/+/#"));
        assert!(matches("sport/tennis", "sport/+/#"));
        assert!(!matches("sport", "sport/+/#"));

        assert!(matches("/finance", "+/+/#"));
        assert!(matches("/finance/", "+/+/#"));
        assert!(matches("/finance/stock", "+/+/#"));
        assert!(matches("/", "+/+/#")); // Actually matches: empty/empty/#
    }

    #[test]
    fn test_edge_cases() {
        // Empty levels
        assert!(matches("/", "/"));
        assert!(matches("/finance", "/finance"));
        assert!(matches("//", "//"));
        assert!(matches("/finance", "/+"));
        assert!(matches("/", "/+")); // + matches empty string
        assert!(!matches("//", "/+")); // /+ has 2 levels, // has 3 levels

        // System topics starting with $ - MQTT spec compliant behavior
        assert!(matches("$SYS/broker/uptime", "$SYS/broker/uptime"));
        assert!(matches("$SYS/broker/uptime", "$SYS/+/uptime"));
        assert!(matches("$SYS/broker/uptime", "$SYS/#"));
        assert!(!matches("$SYS/broker/uptime", "#"));
        assert!(!matches("$SYS/broker/uptime", "+/broker/uptime"));
        assert!(!matches("$SYS/broker/uptime", "+/#"));

        // Long topics
        let long_topic = "a/".repeat(100) + "end";
        let long_filter = "a/".repeat(100) + "end";
        assert!(matches(&long_topic, &long_filter));
        assert!(matches(&long_topic, "#"));

        // $ prefix topics with long paths
        let long_sys_topic = "$".to_string() + &"a/".repeat(100) + "end";
        assert!(!matches(&long_sys_topic, "#"));
    }

    #[test]
    fn test_dollar_prefix_wildcard_exclusion() {
        // MQTT spec: Topics starting with $ should NOT match root-level wildcards
        // This prevents system topics from being accidentally received

        // $ topics should NOT match # at root
        assert!(!matches("$SYS/broker/uptime", "#"));
        assert!(!matches("$data/sensor/temp", "#"));
        assert!(!matches("$", "#"));

        // $ topics should NOT match + at root
        assert!(!matches("$SYS/broker/uptime", "+/broker/uptime"));
        assert!(!matches("$data/sensor/temp", "+/sensor/temp"));
        assert!(!matches("$SYS", "+"));

        // $ topics should NOT match combinations starting with wildcards
        assert!(!matches("$SYS/broker/uptime", "+/#"));
        assert!(!matches("$SYS/broker/uptime", "+/+/uptime"));

        // $ topics SHOULD match when explicitly subscribed with $
        assert!(matches("$SYS/broker/uptime", "$SYS/broker/uptime"));
        assert!(matches("$SYS/broker/uptime", "$SYS/+/uptime"));
        assert!(matches("$SYS/broker/uptime", "$SYS/#"));
        assert!(matches("$data/sensor/temp", "$data/#"));
        assert!(matches("$SYS/broker/uptime", "$SYS/broker/+"));

        // Non-$ topics should still match # and +
        assert!(matches("SYS/broker/uptime", "#"));
        assert!(matches("data/sensor/temp", "+/sensor/temp"));
        assert!(matches("normal/topic", "#"));

        // Edge case: topic with $ not at start should match wildcards
        assert!(matches("prefix/$SYS/data", "#"));
        assert!(matches("prefix/$SYS/data", "+/$SYS/data"));
        assert!(matches("prefix/$SYS/data", "prefix/#"));
    }

    #[test]
    fn test_invalid_inputs() {
        // Invalid topics
        assert!(!matches("sport/tennis+", "sport/tennis+"));
        assert!(!matches("sport/tennis#", "sport/tennis#"));
        assert!(!matches("", "")); // Empty filter is invalid
        assert!(!matches("sport\0tennis", "sport\0tennis"));

        // Invalid filters
        assert!(!matches("sport/tennis", "sport/tennis/#/extra"));
        assert!(!matches("sport/tennis", "sport/+tennis"));
        assert!(!matches("sport/tennis", "sport/#extra"));
    }

    #[test]
    fn test_validation() {
        // Valid topics
        assert!(is_valid_topic("sport/tennis"));
        assert!(is_valid_topic("sport"));
        assert!(is_valid_topic("/"));
        assert!(is_valid_topic("a"));

        // Invalid topics
        assert!(is_valid_topic("")); // Empty is valid
        assert!(!is_valid_topic("sport/+"));
        assert!(!is_valid_topic("sport/#"));
        assert!(!is_valid_topic("sport\0tennis"));
        assert!(!is_valid_topic(&"a".repeat(65536)));

        // Valid filters
        assert!(is_valid_filter("sport/tennis"));
        assert!(is_valid_filter("sport/+"));
        assert!(is_valid_filter("sport/#"));
        assert!(is_valid_filter("+/+/+"));
        assert!(is_valid_filter("#"));

        // Invalid filters
        assert!(!is_valid_filter(""));
        assert!(!is_valid_filter("sport/+tennis"));
        assert!(!is_valid_filter("sport/#/extra"));
        assert!(!is_valid_filter("sport/tennis#"));
        assert!(!is_valid_filter("sport\0tennis"));
        assert!(!is_valid_filter(&"a".repeat(65536)));
    }

    #[test]
    fn test_error_messages() {
        // Test empty topic validation
        assert!(validate_topic("").is_ok()); // Empty topic is valid in MQTT

        assert_eq!(
            validate_topic("sport/+").unwrap_err().to_string(),
            "Invalid topic name: Invalid topic: wildcards not allowed in topic names"
        );

        assert_eq!(
            validate_filter("sport/+tennis").unwrap_err().to_string(),
            "Invalid topic filter: Invalid filter: invalid wildcard usage"
        );
    }
}
