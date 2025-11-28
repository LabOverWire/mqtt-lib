//! Access Control Lists (ACLs) for MQTT broker topic authorization
//!
//! Provides fine-grained topic-based access control for publish and subscribe operations.
//! Supports pattern matching with wildcards and user-based permissions.

use crate::error::{MqttError, Result};
use crate::validation::topic_matches_filter;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use tokio::fs;
use tokio::sync::RwLock;
use tracing::debug;
#[cfg(not(target_arch = "wasm32"))]
use tracing::{info, warn};

/// Access permissions for topics
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Read access (subscribe)
    Read,
    /// Write access (publish)
    Write,
    /// Both read and write access
    ReadWrite,
    /// Explicitly deny access
    Deny,
}

impl Permission {
    /// Check if this permission allows reading (subscribing)
    #[must_use]
    pub fn allows_read(&self) -> bool {
        matches!(self, Permission::Read | Permission::ReadWrite)
    }

    /// Check if this permission allows writing (publishing)
    #[must_use]
    pub fn allows_write(&self) -> bool {
        matches!(self, Permission::Write | Permission::ReadWrite)
    }

    /// Check if this permission explicitly denies access
    #[must_use]
    pub fn is_deny(&self) -> bool {
        matches!(self, Permission::Deny)
    }
}

impl std::str::FromStr for Permission {
    type Err = MqttError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "read" | "subscribe" => Ok(Permission::Read),
            "write" | "publish" => Ok(Permission::Write),
            "readwrite" | "rw" | "all" => Ok(Permission::ReadWrite),
            "deny" | "none" => Ok(Permission::Deny),
            _ => Err(MqttError::Configuration(format!("Invalid permission: {s}"))),
        }
    }
}

/// ACL rule for a specific user and topic pattern
#[derive(Debug, Clone)]
pub struct AclRule {
    /// Username (or "*" for all users)
    pub username: String,
    /// Topic pattern (supports MQTT wildcards + and #)
    pub topic_pattern: String,
    /// Permission level
    pub permission: Permission,
}

impl AclRule {
    /// Creates a new ACL rule
    #[must_use]
    pub fn new(username: String, topic_pattern: String, permission: Permission) -> Self {
        Self {
            username,
            topic_pattern,
            permission,
        }
    }

    /// Check if this rule matches the given user and topic
    #[must_use]
    pub fn matches(&self, username: Option<&str>, topic: &str) -> bool {
        // Check username match
        let username_matches = match username {
            Some(user) => self.username == "*" || self.username == user,
            None => self.username == "*" || self.username == "anonymous",
        };

        if !username_matches {
            return false;
        }

        // Check topic pattern match
        topic_matches_filter(topic, &self.topic_pattern)
    }
}

/// Access Control List manager
#[derive(Debug)]
pub struct AclManager {
    /// List of ACL rules (evaluated in order)
    rules: Arc<RwLock<Vec<AclRule>>>,
    /// Path to ACL file (optional)
    acl_file: Option<std::path::PathBuf>,
    /// Default permission when no rules match
    default_permission: Permission,
}

impl Default for AclManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AclManager {
    /// Creates a new ACL manager with default permissions
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: Arc::new(RwLock::new(Vec::new())),
            acl_file: None,
            default_permission: Permission::Deny,
        }
    }

    /// Creates an ACL manager that allows all access by default
    #[must_use]
    pub fn allow_all() -> Self {
        Self {
            rules: Arc::new(RwLock::new(Vec::new())),
            acl_file: None,
            default_permission: Permission::ReadWrite,
        }
    }

    /// Creates an ACL manager from a file
    ///
    /// File format (one rule per line):
    /// ```text
    /// # Comments start with #
    /// user alice topic sensors/+ permission read
    /// user bob topic actuators/# permission write
    /// user * topic public/# permission readwrite
    /// user alice topic private/alice/# permission readwrite
    /// user * topic admin/# permission deny
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let manager = Self {
            rules: Arc::new(RwLock::new(Vec::new())),
            acl_file: Some(path.clone()),
            default_permission: Permission::Deny,
        };

        manager.load_acl_file().await?;
        Ok(manager)
    }

    /// Sets the default permission for when no rules match
    #[must_use]
    pub fn with_default_permission(mut self, permission: Permission) -> Self {
        self.default_permission = permission;
        self
    }

    /// Loads or reloads the ACL file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn load_acl_file(&self) -> Result<()> {
        let Some(ref path) = self.acl_file else {
            return Ok(());
        };

        let content = fs::read_to_string(path).await.map_err(|e| {
            MqttError::Configuration(format!("Failed to read ACL file {}: {e}", path.display()))
        })?;

        let mut rules = Vec::new();
        let mut line_num = 0;

        for line in content.lines() {
            line_num += 1;
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse ACL rule format: user <username> topic <topic_pattern> permission <permission>
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() != 6 {
                warn!("Invalid ACL rule format at line {line_num}: {line}");
                continue;
            }

            if parts[0] != "user" || parts[2] != "topic" || parts[4] != "permission" {
                warn!("Invalid ACL rule format at line {line_num}: {line}");
                continue;
            }

            let username = parts[1].to_string();
            let topic_pattern = parts[3].to_string();
            let permission = match parts[5].parse::<Permission>() {
                Ok(p) => p,
                Err(e) => {
                    warn!("Invalid permission at line {line_num}: {} - {e}", parts[5]);
                    continue;
                }
            };

            rules.push(AclRule::new(username, topic_pattern, permission));
        }

        // Update the rules atomically
        *self.rules.write().await = rules;

        info!(
            "Loaded {} ACL rules from file: {}",
            self.rules.read().await.len(),
            path.display()
        );
        Ok(())
    }

    /// Reloads the ACL file (alias for load_acl_file).
    ///
    /// # Errors
    /// Returns an error if the ACL file cannot be read or parsed.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn reload(&self) -> Result<()> {
        self.load_acl_file().await
    }

    /// Adds an ACL rule
    pub async fn add_rule(&self, rule: AclRule) {
        self.rules.write().await.push(rule);
    }

    /// Removes all ACL rules
    pub async fn clear_rules(&self) {
        self.rules.write().await.clear();
    }

    /// Gets the number of ACL rules
    pub async fn rule_count(&self) -> usize {
        self.rules.read().await.len()
    }

    /// Check if a user is authorized to publish to a topic
    pub async fn check_publish(&self, username: Option<&str>, topic: &str) -> bool {
        self.check_permission(username, topic, |p| p.allows_write())
            .await
    }

    /// Check if a user is authorized to subscribe to a topic filter
    pub async fn check_subscribe(&self, username: Option<&str>, topic_filter: &str) -> bool {
        self.check_permission(username, topic_filter, |p| p.allows_read())
            .await
    }

    /// Check permission with a custom predicate
    async fn check_permission<F>(&self, username: Option<&str>, topic: &str, check: F) -> bool
    where
        F: Fn(Permission) -> bool,
    {
        let rules = self.rules.read().await;

        // Find the most specific matching rule
        // Priority: exact username > wildcard username
        // For same username priority: more specific topic pattern > less specific
        let mut best_match: Option<&AclRule> = None;
        let mut best_specificity = -1i32;

        for rule in rules.iter() {
            if rule.matches(username, topic) {
                // Calculate rule specificity score
                let mut specificity = 0;

                // Exact username is more specific than wildcard
                if rule.username != "*" {
                    specificity += 100;
                }

                // More specific topic patterns get higher scores
                // Fewer wildcards = more specific
                let wildcard_count = rule
                    .topic_pattern
                    .chars()
                    .filter(|&c| c == '+' || c == '#')
                    .count();
                specificity += 50 - (i32::try_from(wildcard_count).unwrap_or(i32::MAX) * 10);

                // Topic length as tiebreaker (longer = more specific)
                specificity += i32::try_from(rule.topic_pattern.len()).unwrap_or(i32::MAX);

                if specificity > best_specificity {
                    best_match = Some(rule);
                    best_specificity = specificity;
                }
            }
        }

        // Apply the best matching rule
        if let Some(rule) = best_match {
            debug!(
                "ACL rule matched: user={:?}, topic={}, permission={:?}, specificity={}",
                username, topic, rule.permission, best_specificity
            );

            // If explicitly denied, deny immediately
            if rule.permission.is_deny() {
                return false;
            }

            // Check if permission allows the requested operation
            return check(rule.permission);
        }

        // No matching rule found, use default permission
        debug!(
            "No ACL rule matched for user={:?}, topic={}, using default permission={:?}",
            username, topic, self.default_permission
        );

        if self.default_permission.is_deny() {
            false
        } else {
            check(self.default_permission)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_parsing() {
        assert_eq!("read".parse::<Permission>().unwrap(), Permission::Read);
        assert_eq!("write".parse::<Permission>().unwrap(), Permission::Write);
        assert_eq!(
            "readwrite".parse::<Permission>().unwrap(),
            Permission::ReadWrite
        );
        assert_eq!("deny".parse::<Permission>().unwrap(), Permission::Deny);
        assert_eq!("subscribe".parse::<Permission>().unwrap(), Permission::Read);
        assert_eq!("publish".parse::<Permission>().unwrap(), Permission::Write);
        assert_eq!("all".parse::<Permission>().unwrap(), Permission::ReadWrite);

        assert!("invalid".parse::<Permission>().is_err());
    }

    #[test]
    fn test_permission_checks() {
        assert!(Permission::Read.allows_read());
        assert!(!Permission::Read.allows_write());

        assert!(!Permission::Write.allows_read());
        assert!(Permission::Write.allows_write());

        assert!(Permission::ReadWrite.allows_read());
        assert!(Permission::ReadWrite.allows_write());

        assert!(!Permission::Deny.allows_read());
        assert!(!Permission::Deny.allows_write());
        assert!(Permission::Deny.is_deny());
    }

    #[test]
    fn test_acl_rule_matching() {
        let rule = AclRule::new(
            "alice".to_string(),
            "sensors/+".to_string(),
            Permission::Read,
        );

        // Username matching
        assert!(rule.matches(Some("alice"), "sensors/temp"));
        assert!(!rule.matches(Some("bob"), "sensors/temp"));
        assert!(!rule.matches(None, "sensors/temp"));

        // Topic matching
        assert!(rule.matches(Some("alice"), "sensors/temp"));
        assert!(rule.matches(Some("alice"), "sensors/humidity"));
        assert!(!rule.matches(Some("alice"), "actuators/fan"));
        assert!(!rule.matches(Some("alice"), "sensors/temp/room1"));

        // Wildcard user rule
        let wildcard_rule = AclRule::new(
            "*".to_string(),
            "public/#".to_string(),
            Permission::ReadWrite,
        );
        assert!(wildcard_rule.matches(Some("alice"), "public/messages"));
        assert!(wildcard_rule.matches(Some("bob"), "public/status"));
        assert!(wildcard_rule.matches(None, "public/announcements"));
    }

    #[tokio::test]
    async fn test_acl_basic_operations() {
        let acl = AclManager::new();

        // Default deny
        assert!(!acl.check_publish(Some("alice"), "sensors/temp").await);
        assert!(!acl.check_subscribe(Some("alice"), "sensors/+").await);

        // Add rule allowing Alice to read sensors
        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "sensors/+".to_string(),
            Permission::Read,
        ))
        .await;

        assert!(!acl.check_publish(Some("alice"), "sensors/temp").await);
        assert!(acl.check_subscribe(Some("alice"), "sensors/temp").await);

        // Add rule allowing Bob to write to actuators
        acl.add_rule(AclRule::new(
            "bob".to_string(),
            "actuators/#".to_string(),
            Permission::Write,
        ))
        .await;

        assert!(acl.check_publish(Some("bob"), "actuators/fan/speed").await);
        assert!(
            !acl.check_subscribe(Some("bob"), "actuators/fan/speed")
                .await
        );

        // Verify isolation
        assert!(!acl.check_publish(Some("alice"), "actuators/fan").await);
        assert!(!acl.check_subscribe(Some("bob"), "sensors/temp").await);
    }

    #[tokio::test]
    async fn test_acl_rule_priority() {
        let acl = AclManager::new();

        // Add general rule first
        acl.add_rule(AclRule::new(
            "*".to_string(),
            "data/#".to_string(),
            Permission::ReadWrite,
        ))
        .await;

        // Add specific deny rule
        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "data/secret/#".to_string(),
            Permission::Deny,
        ))
        .await;

        // Alice should be denied access to secret data (specific rule wins)
        assert!(!acl.check_publish(Some("alice"), "data/secret/file1").await);
        assert!(
            !acl.check_subscribe(Some("alice"), "data/secret/file1")
                .await
        );

        // Alice should have access to other data
        assert!(acl.check_publish(Some("alice"), "data/public/file1").await);
        assert!(
            acl.check_subscribe(Some("alice"), "data/public/file1")
                .await
        );

        // Bob should have access to all data including secret
        assert!(acl.check_publish(Some("bob"), "data/secret/file1").await);
        assert!(acl.check_subscribe(Some("bob"), "data/secret/file1").await);
    }

    #[tokio::test]
    async fn test_acl_allow_all_manager() {
        let acl = AclManager::allow_all();

        // Should allow everything by default
        assert!(acl.check_publish(Some("alice"), "any/topic").await);
        assert!(acl.check_subscribe(Some("alice"), "any/topic").await);
        assert!(acl.check_publish(None, "anonymous/topic").await);

        // Add specific deny rule
        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "forbidden/#".to_string(),
            Permission::Deny,
        ))
        .await;

        // Should deny Alice access to forbidden topics
        assert!(!acl.check_publish(Some("alice"), "forbidden/secret").await);
        assert!(!acl.check_subscribe(Some("alice"), "forbidden/secret").await);

        // Should still allow Alice access to other topics
        assert!(acl.check_publish(Some("alice"), "allowed/topic").await);
        assert!(acl.check_subscribe(Some("alice"), "allowed/topic").await);
    }

    #[tokio::test]
    async fn test_acl_file_loading() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create temporary ACL file
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "# ACL file").unwrap();
        writeln!(temp_file, "user alice topic sensors/+ permission read").unwrap();
        writeln!(temp_file, "user bob topic actuators/# permission write").unwrap();
        writeln!(temp_file, "user * topic public/# permission readwrite").unwrap();
        writeln!(temp_file, "user alice topic admin/# permission deny").unwrap();
        writeln!(temp_file).unwrap(); // Empty line
        writeln!(temp_file, "# Another comment").unwrap();
        writeln!(temp_file, "invalid line format").unwrap();
        temp_file.flush().unwrap();

        // Load from file
        let acl = AclManager::from_file(temp_file.path()).await.unwrap();
        assert_eq!(acl.rule_count().await, 4);

        // Test Alice's permissions
        assert!(!acl.check_publish(Some("alice"), "sensors/temp").await);
        assert!(acl.check_subscribe(Some("alice"), "sensors/temp").await);
        assert!(
            acl.check_publish(Some("alice"), "public/announcements")
                .await
        );
        assert!(!acl.check_publish(Some("alice"), "admin/users").await);

        // Test Bob's permissions
        assert!(acl.check_publish(Some("bob"), "actuators/fan").await);
        assert!(!acl.check_subscribe(Some("bob"), "actuators/fan").await);
        assert!(acl.check_publish(Some("bob"), "public/messages").await);

        // Test unknown user (should get wildcard permissions)
        assert!(acl.check_publish(Some("charlie"), "public/chat").await);
        assert!(!acl.check_publish(Some("charlie"), "sensors/temp").await);
    }
}
