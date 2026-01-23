use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChangeOnlyDeliveryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub topic_patterns: Vec<String>,
}

impl ChangeOnlyDeliveryConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[must_use]
    pub fn with_topic_patterns(mut self, patterns: Vec<String>) -> Self {
        self.topic_patterns = patterns;
        self
    }

    #[must_use]
    pub fn add_topic_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.topic_patterns.push(pattern.into());
        self
    }
}
