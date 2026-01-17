//! ACL rule types and permission definitions

use crate::error::{MqttError, Result};
use crate::validation::topic_matches_filter;
use std::collections::HashSet;

/// Access permissions for topics
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Read,
    Write,
    ReadWrite,
    Deny,
}

impl Permission {
    #[must_use]
    pub fn allows_read(&self) -> bool {
        matches!(self, Permission::Read | Permission::ReadWrite)
    }

    #[must_use]
    pub fn allows_write(&self) -> bool {
        matches!(self, Permission::Write | Permission::ReadWrite)
    }

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
    pub username: String,
    pub topic_pattern: String,
    pub permission: Permission,
}

impl AclRule {
    #[must_use]
    pub fn new(username: String, topic_pattern: String, permission: Permission) -> Self {
        Self {
            username,
            topic_pattern,
            permission,
        }
    }

    #[must_use]
    pub fn matches(&self, username: Option<&str>, topic: &str) -> bool {
        let username_matches = match username {
            Some(user) => self.username == "*" || self.username == user,
            None => self.username == "*" || self.username == "anonymous",
        };

        if !username_matches {
            return false;
        }

        topic_matches_filter(topic, &self.topic_pattern)
    }
}

/// A rule within a role (topic pattern and permission)
#[derive(Debug, Clone)]
pub struct RoleRule {
    pub topic_pattern: String,
    pub permission: Permission,
}

impl RoleRule {
    #[must_use]
    pub fn new(topic_pattern: String, permission: Permission) -> Self {
        Self {
            topic_pattern,
            permission,
        }
    }

    #[must_use]
    pub fn matches(&self, topic: &str) -> bool {
        topic_matches_filter(topic, &self.topic_pattern)
    }
}

/// A named role containing a set of topic/permission rules
#[derive(Debug, Clone)]
pub struct Role {
    pub name: String,
    pub rules: Vec<RoleRule>,
}

impl Role {
    #[must_use]
    pub fn new(name: String) -> Self {
        Self {
            name,
            rules: Vec::new(),
        }
    }

    pub fn add_rule(&mut self, rule: RoleRule) {
        self.rules.push(rule);
    }

    pub fn remove_rule(&mut self, topic_pattern: &str) -> bool {
        let len_before = self.rules.len();
        self.rules.retain(|r| r.topic_pattern != topic_pattern);
        self.rules.len() < len_before
    }
}

/// Entry for federated (JWT-derived) roles with metadata
#[derive(Debug, Clone)]
pub struct FederatedRoleEntry {
    pub roles: HashSet<String>,
    pub issuer: String,
    pub mode: crate::broker::config::FederatedAuthMode,
    pub session_bound: bool,
}

impl FederatedRoleEntry {
    #[must_use]
    pub fn new(
        roles: HashSet<String>,
        issuer: String,
        mode: crate::broker::config::FederatedAuthMode,
        session_bound: bool,
    ) -> Self {
        Self {
            roles,
            issuer,
            mode,
            session_bound,
        }
    }
}
