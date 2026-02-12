use serde::{Deserialize, Serialize};

fn default_property_key() -> String {
    "x-origin-client-id".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoSuppressionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_property_key")]
    pub property_key: String,
}

impl Default for EchoSuppressionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            property_key: default_property_key(),
        }
    }
}

impl EchoSuppressionConfig {
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
    pub fn with_property_key(mut self, key: impl Into<String>) -> Self {
        self.property_key = key.into();
        self
    }
}
