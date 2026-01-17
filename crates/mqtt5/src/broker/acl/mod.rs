//! Access Control Lists (ACLs) for MQTT broker topic authorization
//!
//! Provides fine-grained topic-based access control for publish and subscribe operations.
//! Supports pattern matching with wildcards, user-based permissions, and role-based access control (RBAC).

mod rules;

pub use rules::{AclRule, FederatedRoleEntry, Permission, Role, RoleRule};

use crate::error::{MqttError, Result};
use std::collections::{HashMap, HashSet};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use tokio::fs;
use tokio::sync::RwLock;
use tracing::debug;
#[cfg(not(target_arch = "wasm32"))]
use tracing::{info, warn};

/// Access Control List manager with RBAC support
#[derive(Debug)]
pub struct AclManager {
    rules: Arc<RwLock<Vec<AclRule>>>,
    roles: Arc<RwLock<HashMap<String, Role>>>,
    user_roles: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    federated_user_roles: Arc<RwLock<HashMap<String, FederatedRoleEntry>>>,
    #[cfg(not(target_arch = "wasm32"))]
    acl_file: Option<std::path::PathBuf>,
    default_permission: RwLock<Permission>,
}

impl Default for AclManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AclManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: Arc::new(RwLock::new(Vec::new())),
            roles: Arc::new(RwLock::new(HashMap::new())),
            user_roles: Arc::new(RwLock::new(HashMap::new())),
            federated_user_roles: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(target_arch = "wasm32"))]
            acl_file: None,
            default_permission: RwLock::new(Permission::Deny),
        }
    }

    #[must_use]
    pub fn allow_all() -> Self {
        Self {
            rules: Arc::new(RwLock::new(Vec::new())),
            roles: Arc::new(RwLock::new(HashMap::new())),
            user_roles: Arc::new(RwLock::new(HashMap::new())),
            federated_user_roles: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(target_arch = "wasm32"))]
            acl_file: None,
            default_permission: RwLock::new(Permission::ReadWrite),
        }
    }

    /// Creates an ACL manager from a file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let manager = Self {
            rules: Arc::new(RwLock::new(Vec::new())),
            roles: Arc::new(RwLock::new(HashMap::new())),
            user_roles: Arc::new(RwLock::new(HashMap::new())),
            federated_user_roles: Arc::new(RwLock::new(HashMap::new())),
            acl_file: Some(path.clone()),
            default_permission: RwLock::new(Permission::Deny),
        };

        manager.load_acl_file().await?;
        Ok(manager)
    }

    #[must_use]
    pub fn with_default_permission(mut self, permission: Permission) -> Self {
        self.default_permission = RwLock::new(permission);
        self
    }

    pub async fn set_default_permission(&self, permission: Permission) {
        *self.default_permission.write().await = permission;
    }

    pub async fn get_default_permission(&self) -> Permission {
        *self.default_permission.read().await
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

        let mut acl_rules = Vec::new();
        let mut role_defs: HashMap<String, Role> = HashMap::new();
        let mut user_role_assignments: HashMap<String, HashSet<String>> = HashMap::new();
        let mut line_num = 0;

        for line in content.lines() {
            line_num += 1;
            let line = line.trim();

            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            match parts[0] {
                "user" => {
                    if parts.len() != 6 || parts[2] != "topic" || parts[4] != "permission" {
                        warn!("Invalid user rule format at line {line_num}: {line}");
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
                    acl_rules.push(AclRule::new(username, topic_pattern, permission));
                }
                "role" => {
                    if parts.len() != 6 || parts[2] != "topic" || parts[4] != "permission" {
                        warn!("Invalid role rule format at line {line_num}: {line}");
                        continue;
                    }
                    let role_name = parts[1].to_string();
                    let topic_pattern = parts[3].to_string();
                    let permission = match parts[5].parse::<Permission>() {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("Invalid permission at line {line_num}: {} - {e}", parts[5]);
                            continue;
                        }
                    };
                    let role = role_defs
                        .entry(role_name.clone())
                        .or_insert_with(|| Role::new(role_name));
                    role.add_rule(RoleRule::new(topic_pattern, permission));
                }
                "assign" => {
                    if parts.len() != 3 {
                        warn!("Invalid assign format at line {line_num}: {line}");
                        continue;
                    }
                    let username = parts[1].to_string();
                    let role_name = parts[2].to_string();
                    user_role_assignments
                        .entry(username)
                        .or_default()
                        .insert(role_name);
                }
                _ => {
                    warn!("Unknown directive at line {line_num}: {line}");
                }
            }
        }

        *self.rules.write().await = acl_rules;
        *self.roles.write().await = role_defs;
        *self.user_roles.write().await = user_role_assignments;

        info!(
            "Loaded {} user rules, {} roles, {} user-role assignments from file: {}",
            self.rules.read().await.len(),
            self.roles.read().await.len(),
            self.user_roles.read().await.len(),
            path.display()
        );
        Ok(())
    }

    /// Reloads the ACL file
    ///
    /// # Errors
    ///
    /// Returns an error if the ACL file cannot be read or parsed
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn reload(&self) -> Result<()> {
        self.load_acl_file().await
    }

    pub async fn add_rule(&self, rule: AclRule) {
        self.rules.write().await.push(rule);
    }

    pub async fn clear_rules(&self) {
        self.rules.write().await.clear();
    }

    pub async fn rule_count(&self) -> usize {
        self.rules.read().await.len()
    }

    pub async fn add_role(&self, name: String) {
        let mut roles = self.roles.write().await;
        roles.entry(name.clone()).or_insert_with(|| Role::new(name));
    }

    pub async fn remove_role(&self, name: &str) -> bool {
        let removed = self.roles.write().await.remove(name).is_some();
        if removed {
            let mut user_roles = self.user_roles.write().await;
            for assignments in user_roles.values_mut() {
                assignments.remove(name);
            }
        }
        removed
    }

    pub async fn get_role(&self, name: &str) -> Option<Role> {
        self.roles.read().await.get(name).cloned()
    }

    pub async fn list_roles(&self) -> Vec<String> {
        self.roles.read().await.keys().cloned().collect()
    }

    pub async fn role_count(&self) -> usize {
        self.roles.read().await.len()
    }

    /// Adds a rule to a role
    ///
    /// # Errors
    ///
    /// Currently infallible, returns `Ok(())` always.
    pub async fn add_role_rule(
        &self,
        role_name: &str,
        topic_pattern: String,
        permission: Permission,
    ) -> Result<()> {
        let mut roles = self.roles.write().await;
        let role = roles
            .entry(role_name.to_string())
            .or_insert_with(|| Role::new(role_name.to_string()));
        role.add_rule(RoleRule::new(topic_pattern, permission));
        Ok(())
    }

    pub async fn remove_role_rule(&self, role_name: &str, topic_pattern: &str) -> bool {
        let mut roles = self.roles.write().await;
        if let Some(role) = roles.get_mut(role_name) {
            role.remove_rule(topic_pattern)
        } else {
            false
        }
    }

    /// Assigns a user to a role
    ///
    /// # Errors
    ///
    /// Returns an error if the role does not exist.
    pub async fn assign_role(&self, username: &str, role_name: &str) -> Result<()> {
        if !self.roles.read().await.contains_key(role_name) {
            return Err(MqttError::Configuration(format!(
                "Role '{role_name}' does not exist"
            )));
        }
        self.user_roles
            .write()
            .await
            .entry(username.to_string())
            .or_default()
            .insert(role_name.to_string());
        Ok(())
    }

    pub async fn unassign_role(&self, username: &str, role_name: &str) -> bool {
        let mut user_roles = self.user_roles.write().await;
        if let Some(roles) = user_roles.get_mut(username) {
            roles.remove(role_name)
        } else {
            false
        }
    }

    pub async fn get_user_roles(&self, username: &str) -> Vec<String> {
        self.user_roles
            .read()
            .await
            .get(username)
            .map(|roles| roles.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn get_role_users(&self, role_name: &str) -> Vec<String> {
        self.user_roles
            .read()
            .await
            .iter()
            .filter_map(|(user, roles)| {
                if roles.contains(role_name) {
                    Some(user.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub async fn clear_roles(&self) {
        self.roles.write().await.clear();
        self.user_roles.write().await.clear();
    }

    pub async fn set_federated_roles(&self, user_id: &str, entry: FederatedRoleEntry) {
        self.federated_user_roles
            .write()
            .await
            .insert(user_id.to_string(), entry);
    }

    pub async fn get_federated_roles(&self, user_id: &str) -> Option<FederatedRoleEntry> {
        self.federated_user_roles.read().await.get(user_id).cloned()
    }

    pub async fn clear_federated_roles(&self, user_id: &str) {
        self.federated_user_roles.write().await.remove(user_id);
    }

    pub async fn clear_session_bound_roles(&self, user_id: &str) {
        let mut fed_roles = self.federated_user_roles.write().await;
        if let Some(entry) = fed_roles.get(user_id) {
            if entry.session_bound {
                fed_roles.remove(user_id);
            }
        }
    }

    pub async fn check_publish(&self, username: Option<&str>, topic: &str) -> bool {
        self.check_permission(username, topic, |p| p.allows_write())
            .await
    }

    pub async fn check_subscribe(&self, username: Option<&str>, topic_filter: &str) -> bool {
        self.check_permission(username, topic_filter, |p| p.allows_read())
            .await
    }

    async fn check_permission<F>(&self, username: Option<&str>, topic: &str, check: F) -> bool
    where
        F: Fn(Permission) -> bool,
    {
        let rules = self.rules.read().await;

        let (direct_match, direct_specificity) =
            Self::find_best_direct_rule(&rules, username, topic, false);

        if let Some(rule) = direct_match {
            debug!(
                "Direct user rule matched: user={:?}, topic={}, permission={:?}, specificity={}",
                username, topic, rule.permission, direct_specificity
            );

            if rule.permission.is_deny() {
                return false;
            }

            if check(rule.permission) {
                return true;
            }
        }

        if let Some(user) = username {
            let user_roles_map = self.user_roles.read().await;
            let fed_roles_map = self.federated_user_roles.read().await;

            let mut all_assigned_roles: HashSet<String> =
                user_roles_map.get(user).cloned().unwrap_or_default();

            if let Some(fed_entry) = fed_roles_map.get(user) {
                all_assigned_roles.extend(fed_entry.roles.iter().cloned());
            }

            if !all_assigned_roles.is_empty() {
                let roles_map = self.roles.read().await;
                let mut found_deny = false;
                let mut found_allow = false;

                for role_name in &all_assigned_roles {
                    if let Some(role) = roles_map.get(role_name) {
                        for rule in &role.rules {
                            if rule.matches(topic) {
                                if rule.permission.is_deny() {
                                    debug!(
                                        "Role deny matched: user={}, role={}, topic={}, pattern={}",
                                        user, role_name, topic, rule.topic_pattern
                                    );
                                    found_deny = true;
                                } else if check(rule.permission) {
                                    debug!(
                                        "Role allow matched: user={}, role={}, topic={}, pattern={}, permission={:?}",
                                        user, role_name, topic, rule.topic_pattern, rule.permission
                                    );
                                    found_allow = true;
                                }
                            }
                        }
                    }
                }

                if found_deny {
                    return false;
                }

                if found_allow {
                    return true;
                }
            }
        }

        let (wildcard_match, wildcard_specificity) =
            Self::find_best_direct_rule(&rules, username, topic, true);

        if let Some(rule) = wildcard_match {
            debug!(
                "Wildcard user rule matched: user={:?}, topic={}, permission={:?}, specificity={}",
                username, topic, rule.permission, wildcard_specificity
            );

            if rule.permission.is_deny() {
                return false;
            }

            return check(rule.permission);
        }

        let default_perm = *self.default_permission.read().await;
        debug!(
            "No ACL rule matched for user={:?}, topic={}, using default permission={:?}",
            username, topic, default_perm
        );

        if default_perm.is_deny() {
            false
        } else {
            check(default_perm)
        }
    }

    fn find_best_direct_rule<'a>(
        rules: &'a [AclRule],
        username: Option<&str>,
        topic: &str,
        wildcard_only: bool,
    ) -> (Option<&'a AclRule>, i32) {
        let mut best_match: Option<&AclRule> = None;
        let mut best_specificity = -1i32;

        for rule in rules {
            let is_wildcard = rule.username == "*";

            if wildcard_only && !is_wildcard {
                continue;
            }
            if !wildcard_only && is_wildcard {
                continue;
            }

            if rule.matches(username, topic) {
                let mut specificity = 0;

                if !is_wildcard {
                    specificity += 100;
                }

                let wildcard_count = rule
                    .topic_pattern
                    .chars()
                    .filter(|&c| c == '+' || c == '#')
                    .count();
                specificity += 50 - (i32::try_from(wildcard_count).unwrap_or(i32::MAX) * 10);
                specificity += i32::try_from(rule.topic_pattern.len()).unwrap_or(i32::MAX);

                if specificity > best_specificity {
                    best_match = Some(rule);
                    best_specificity = specificity;
                }
            }
        }

        (best_match, best_specificity)
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

        assert!(rule.matches(Some("alice"), "sensors/temp"));
        assert!(!rule.matches(Some("bob"), "sensors/temp"));
        assert!(!rule.matches(None, "sensors/temp"));

        assert!(rule.matches(Some("alice"), "sensors/temp"));
        assert!(rule.matches(Some("alice"), "sensors/humidity"));
        assert!(!rule.matches(Some("alice"), "actuators/fan"));
        assert!(!rule.matches(Some("alice"), "sensors/temp/room1"));

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

        assert!(!acl.check_publish(Some("alice"), "sensors/temp").await);
        assert!(!acl.check_subscribe(Some("alice"), "sensors/+").await);

        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "sensors/+".to_string(),
            Permission::Read,
        ))
        .await;

        assert!(!acl.check_publish(Some("alice"), "sensors/temp").await);
        assert!(acl.check_subscribe(Some("alice"), "sensors/temp").await);

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

        assert!(!acl.check_publish(Some("alice"), "actuators/fan").await);
        assert!(!acl.check_subscribe(Some("bob"), "sensors/temp").await);
    }

    #[tokio::test]
    async fn test_acl_rule_priority() {
        let acl = AclManager::new();

        acl.add_rule(AclRule::new(
            "*".to_string(),
            "data/#".to_string(),
            Permission::ReadWrite,
        ))
        .await;

        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "data/secret/#".to_string(),
            Permission::Deny,
        ))
        .await;

        assert!(!acl.check_publish(Some("alice"), "data/secret/file1").await);
        assert!(
            !acl.check_subscribe(Some("alice"), "data/secret/file1")
                .await
        );

        assert!(acl.check_publish(Some("alice"), "data/public/file1").await);
        assert!(
            acl.check_subscribe(Some("alice"), "data/public/file1")
                .await
        );

        assert!(acl.check_publish(Some("bob"), "data/secret/file1").await);
        assert!(acl.check_subscribe(Some("bob"), "data/secret/file1").await);
    }

    #[tokio::test]
    async fn test_acl_allow_all_manager() {
        let acl = AclManager::allow_all();

        assert!(acl.check_publish(Some("alice"), "any/topic").await);
        assert!(acl.check_subscribe(Some("alice"), "any/topic").await);
        assert!(acl.check_publish(None, "anonymous/topic").await);

        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "forbidden/#".to_string(),
            Permission::Deny,
        ))
        .await;

        assert!(!acl.check_publish(Some("alice"), "forbidden/secret").await);
        assert!(!acl.check_subscribe(Some("alice"), "forbidden/secret").await);

        assert!(acl.check_publish(Some("alice"), "allowed/topic").await);
        assert!(acl.check_subscribe(Some("alice"), "allowed/topic").await);
    }

    #[tokio::test]
    async fn test_acl_file_loading() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "# ACL file").unwrap();
        writeln!(temp_file, "user alice topic sensors/+ permission read").unwrap();
        writeln!(temp_file, "user bob topic actuators/# permission write").unwrap();
        writeln!(temp_file, "user * topic public/# permission readwrite").unwrap();
        writeln!(temp_file, "user alice topic admin/# permission deny").unwrap();
        writeln!(temp_file).unwrap();
        writeln!(temp_file, "# Another comment").unwrap();
        writeln!(temp_file, "invalid line format").unwrap();
        temp_file.flush().unwrap();

        let acl = AclManager::from_file(temp_file.path()).await.unwrap();
        assert_eq!(acl.rule_count().await, 4);

        assert!(!acl.check_publish(Some("alice"), "sensors/temp").await);
        assert!(acl.check_subscribe(Some("alice"), "sensors/temp").await);
        assert!(
            acl.check_publish(Some("alice"), "public/announcements")
                .await
        );
        assert!(!acl.check_publish(Some("alice"), "admin/users").await);

        assert!(acl.check_publish(Some("bob"), "actuators/fan").await);
        assert!(!acl.check_subscribe(Some("bob"), "actuators/fan").await);
        assert!(acl.check_publish(Some("bob"), "public/messages").await);

        assert!(acl.check_publish(Some("charlie"), "public/chat").await);
        assert!(!acl.check_publish(Some("charlie"), "sensors/temp").await);
    }

    #[tokio::test]
    async fn test_role_management() {
        let acl = AclManager::new();

        acl.add_role("admin".to_string()).await;
        acl.add_role("reader".to_string()).await;

        assert_eq!(acl.role_count().await, 2);
        assert!(acl.get_role("admin").await.is_some());
        assert!(acl.get_role("nonexistent").await.is_none());

        let roles = acl.list_roles().await;
        assert!(roles.contains(&"admin".to_string()));
        assert!(roles.contains(&"reader".to_string()));

        assert!(acl.remove_role("admin").await);
        assert_eq!(acl.role_count().await, 1);
        assert!(!acl.remove_role("admin").await);
    }

    #[tokio::test]
    async fn test_role_rules() {
        let acl = AclManager::new();

        acl.add_role_rule("admin", "admin/#".to_string(), Permission::ReadWrite)
            .await
            .unwrap();
        acl.add_role_rule("admin", "$SYS/#".to_string(), Permission::Read)
            .await
            .unwrap();

        let role = acl.get_role("admin").await.unwrap();
        assert_eq!(role.rules.len(), 2);

        assert!(acl.remove_role_rule("admin", "admin/#").await);
        let role = acl.get_role("admin").await.unwrap();
        assert_eq!(role.rules.len(), 1);
    }

    #[tokio::test]
    async fn test_user_role_assignment() {
        let acl = AclManager::new();

        acl.add_role("admin".to_string()).await;
        acl.add_role("reader".to_string()).await;

        acl.assign_role("alice", "admin").await.unwrap();
        acl.assign_role("alice", "reader").await.unwrap();
        acl.assign_role("bob", "reader").await.unwrap();

        let alice_roles = acl.get_user_roles("alice").await;
        assert_eq!(alice_roles.len(), 2);
        assert!(alice_roles.contains(&"admin".to_string()));
        assert!(alice_roles.contains(&"reader".to_string()));

        let bob_roles = acl.get_user_roles("bob").await;
        assert_eq!(bob_roles.len(), 1);

        let admin_users = acl.get_role_users("admin").await;
        assert_eq!(admin_users, vec!["alice".to_string()]);

        assert!(acl.unassign_role("alice", "admin").await);
        let alice_roles = acl.get_user_roles("alice").await;
        assert_eq!(alice_roles.len(), 1);

        assert!(acl.assign_role("charlie", "nonexistent").await.is_err());
    }

    #[tokio::test]
    async fn test_rbac_permission_check() {
        let acl = AclManager::new();

        acl.add_role_rule("sensor-reader", "sensors/#".to_string(), Permission::Read)
            .await
            .unwrap();
        acl.add_role_rule(
            "actuator-writer",
            "actuators/#".to_string(),
            Permission::Write,
        )
        .await
        .unwrap();

        acl.assign_role("alice", "sensor-reader").await.unwrap();
        acl.assign_role("bob", "actuator-writer").await.unwrap();
        acl.assign_role("charlie", "sensor-reader").await.unwrap();
        acl.assign_role("charlie", "actuator-writer").await.unwrap();

        assert!(acl.check_subscribe(Some("alice"), "sensors/temp").await);
        assert!(!acl.check_publish(Some("alice"), "sensors/temp").await);
        assert!(!acl.check_subscribe(Some("alice"), "actuators/fan").await);

        assert!(acl.check_publish(Some("bob"), "actuators/fan").await);
        assert!(!acl.check_subscribe(Some("bob"), "actuators/fan").await);
        assert!(!acl.check_publish(Some("bob"), "sensors/temp").await);

        assert!(acl.check_subscribe(Some("charlie"), "sensors/temp").await);
        assert!(acl.check_publish(Some("charlie"), "actuators/fan").await);
    }

    #[tokio::test]
    async fn test_rbac_deny_overrides_allow() {
        let acl = AclManager::new();

        acl.add_role_rule("full-access", "data/#".to_string(), Permission::ReadWrite)
            .await
            .unwrap();
        acl.add_role_rule("restricted", "data/secret/#".to_string(), Permission::Deny)
            .await
            .unwrap();

        acl.assign_role("alice", "full-access").await.unwrap();
        acl.assign_role("alice", "restricted").await.unwrap();

        assert!(acl.check_publish(Some("alice"), "data/public/file").await);
        assert!(!acl.check_publish(Some("alice"), "data/secret/file").await);
        assert!(!acl.check_subscribe(Some("alice"), "data/secret/file").await);
    }

    #[tokio::test]
    async fn test_direct_rule_overrides_role() {
        let acl = AclManager::new();

        acl.add_role_rule("reader", "docs/#".to_string(), Permission::Read)
            .await
            .unwrap();
        acl.assign_role("alice", "reader").await.unwrap();

        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "docs/private/#".to_string(),
            Permission::Deny,
        ))
        .await;

        assert!(
            acl.check_subscribe(Some("alice"), "docs/public/readme")
                .await
        );
        assert!(
            !acl.check_subscribe(Some("alice"), "docs/private/secret")
                .await
        );
    }

    #[tokio::test]
    async fn test_rbac_file_loading() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "# Direct user rules").unwrap();
        writeln!(temp_file, "user superadmin topic # permission readwrite").unwrap();
        writeln!(temp_file).unwrap();
        writeln!(temp_file, "# Role definitions").unwrap();
        writeln!(temp_file, "role admin topic admin/# permission readwrite").unwrap();
        writeln!(temp_file, "role admin topic $SYS/# permission read").unwrap();
        writeln!(
            temp_file,
            "role device topic devices/+/telemetry permission write"
        )
        .unwrap();
        writeln!(temp_file, "role monitor topic devices/# permission read").unwrap();
        writeln!(temp_file).unwrap();
        writeln!(temp_file, "# User-role assignments").unwrap();
        writeln!(temp_file, "assign alice admin").unwrap();
        writeln!(temp_file, "assign bob device").unwrap();
        writeln!(temp_file, "assign bob monitor").unwrap();
        temp_file.flush().unwrap();

        let acl = AclManager::from_file(temp_file.path()).await.unwrap();

        assert_eq!(acl.rule_count().await, 1);
        assert_eq!(acl.role_count().await, 3);

        assert!(acl.check_publish(Some("superadmin"), "anything/here").await);

        assert!(acl.check_publish(Some("alice"), "admin/users").await);
        assert!(acl.check_subscribe(Some("alice"), "$SYS/stats").await);
        assert!(!acl.check_publish(Some("alice"), "$SYS/stats").await);

        assert!(
            acl.check_publish(Some("bob"), "devices/sensor1/telemetry")
                .await
        );
        assert!(
            acl.check_subscribe(Some("bob"), "devices/sensor1/status")
                .await
        );
        assert!(
            !acl.check_publish(Some("bob"), "devices/sensor1/status")
                .await
        );

        assert!(!acl.check_publish(Some("charlie"), "admin/users").await);
    }

    #[tokio::test]
    async fn test_remove_role_clears_assignments() {
        let acl = AclManager::new();

        acl.add_role("temp-role".to_string()).await;
        acl.assign_role("alice", "temp-role").await.unwrap();
        acl.assign_role("bob", "temp-role").await.unwrap();

        assert_eq!(acl.get_user_roles("alice").await.len(), 1);
        assert_eq!(acl.get_user_roles("bob").await.len(), 1);

        acl.remove_role("temp-role").await;

        assert_eq!(acl.get_user_roles("alice").await.len(), 0);
        assert_eq!(acl.get_user_roles("bob").await.len(), 0);
    }

    #[tokio::test]
    async fn test_direct_user_allow_overrides_role_deny() {
        let acl = AclManager::new();

        acl.add_role_rule("restrictive", "admin/#".to_string(), Permission::Deny)
            .await
            .unwrap();
        acl.assign_role("alice", "restrictive").await.unwrap();

        assert!(!acl.check_publish(Some("alice"), "admin/users").await);

        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "admin/users".to_string(),
            Permission::ReadWrite,
        ))
        .await;

        assert!(acl.check_publish(Some("alice"), "admin/users").await);
        assert!(acl.check_subscribe(Some("alice"), "admin/users").await);
    }

    #[tokio::test]
    async fn test_multiple_roles_conflicting_specificity() {
        let acl = AclManager::new();

        acl.add_role_rule("broad-reader", "data/#".to_string(), Permission::Read)
            .await
            .unwrap();
        acl.add_role_rule(
            "specific-writer",
            "data/logs/+".to_string(),
            Permission::Write,
        )
        .await
        .unwrap();
        acl.add_role_rule(
            "very-specific-deny",
            "data/logs/secret".to_string(),
            Permission::Deny,
        )
        .await
        .unwrap();

        acl.assign_role("alice", "broad-reader").await.unwrap();
        acl.assign_role("alice", "specific-writer").await.unwrap();
        acl.assign_role("alice", "very-specific-deny")
            .await
            .unwrap();

        assert!(acl.check_subscribe(Some("alice"), "data/public").await);
        assert!(!acl.check_publish(Some("alice"), "data/public").await);

        assert!(acl.check_publish(Some("alice"), "data/logs/access").await);
        assert!(acl.check_subscribe(Some("alice"), "data/logs/access").await);

        assert!(!acl.check_publish(Some("alice"), "data/logs/secret").await);
        assert!(!acl.check_subscribe(Some("alice"), "data/logs/secret").await);
    }

    #[tokio::test]
    async fn test_wildcard_user_with_roles() {
        let acl = AclManager::new();

        acl.add_rule(AclRule::new(
            "*".to_string(),
            "public/#".to_string(),
            Permission::ReadWrite,
        ))
        .await;

        acl.add_role_rule("premium", "premium/#".to_string(), Permission::ReadWrite)
            .await
            .unwrap();
        acl.assign_role("alice", "premium").await.unwrap();

        assert!(acl.check_publish(Some("alice"), "public/messages").await);
        assert!(acl.check_subscribe(Some("alice"), "public/messages").await);

        assert!(acl.check_publish(Some("alice"), "premium/content").await);
        assert!(acl.check_subscribe(Some("alice"), "premium/content").await);

        assert!(acl.check_publish(Some("bob"), "public/messages").await);
        assert!(!acl.check_publish(Some("bob"), "premium/content").await);
    }

    #[tokio::test]
    async fn test_direct_deny_blocks_wildcard_and_role() {
        let acl = AclManager::new();

        acl.add_rule(AclRule::new(
            "*".to_string(),
            "#".to_string(),
            Permission::ReadWrite,
        ))
        .await;

        acl.add_role_rule("admin", "#".to_string(), Permission::ReadWrite)
            .await
            .unwrap();
        acl.assign_role("alice", "admin").await.unwrap();

        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "restricted/#".to_string(),
            Permission::Deny,
        ))
        .await;

        assert!(acl.check_publish(Some("alice"), "normal/topic").await);
        assert!(!acl.check_publish(Some("alice"), "restricted/secret").await);
        assert!(
            !acl.check_subscribe(Some("alice"), "restricted/secret")
                .await
        );
    }

    #[tokio::test]
    async fn test_user_assigned_to_deleted_role() {
        let acl = AclManager::new();

        acl.add_role_rule("deleted-role", "data/#".to_string(), Permission::ReadWrite)
            .await
            .unwrap();
        acl.assign_role("alice", "deleted-role").await.unwrap();

        assert!(acl.check_publish(Some("alice"), "data/file").await);

        acl.remove_role("deleted-role").await;

        assert!(!acl.check_publish(Some("alice"), "data/file").await);
    }

    #[tokio::test]
    async fn test_role_permission_with_multiple_patterns() {
        let acl = AclManager::new();

        acl.add_role_rule(
            "multi-pattern",
            "sensors/+/temp".to_string(),
            Permission::Read,
        )
        .await
        .unwrap();
        acl.add_role_rule(
            "multi-pattern",
            "sensors/+/humidity".to_string(),
            Permission::Read,
        )
        .await
        .unwrap();
        acl.add_role_rule(
            "multi-pattern",
            "actuators/#".to_string(),
            Permission::Write,
        )
        .await
        .unwrap();

        acl.assign_role("alice", "multi-pattern").await.unwrap();

        assert!(
            acl.check_subscribe(Some("alice"), "sensors/room1/temp")
                .await
        );
        assert!(
            acl.check_subscribe(Some("alice"), "sensors/room2/humidity")
                .await
        );
        assert!(!acl.check_publish(Some("alice"), "sensors/room1/temp").await);

        assert!(
            acl.check_publish(Some("alice"), "actuators/fan/speed")
                .await
        );
        assert!(
            !acl.check_subscribe(Some("alice"), "actuators/fan/speed")
                .await
        );
    }

    #[tokio::test]
    async fn test_clear_operations() {
        let acl = AclManager::new();

        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "topic1".to_string(),
            Permission::ReadWrite,
        ))
        .await;

        acl.add_role_rule("role1", "topic2".to_string(), Permission::Read)
            .await
            .unwrap();
        acl.assign_role("alice", "role1").await.unwrap();

        assert_eq!(acl.rule_count().await, 1);
        assert_eq!(acl.role_count().await, 1);

        acl.clear_rules().await;
        assert_eq!(acl.rule_count().await, 0);
        assert!(acl.check_subscribe(Some("alice"), "topic2").await);

        acl.clear_roles().await;
        assert_eq!(acl.role_count().await, 0);
        assert_eq!(acl.get_user_roles("alice").await.len(), 0);
        assert!(!acl.check_subscribe(Some("alice"), "topic2").await);
    }

    #[tokio::test]
    async fn test_file_parsing_malformed_directives() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "user alice topic sensors/+ permission read").unwrap();
        writeln!(temp_file, "role admin topic admin/# permission readwrite").unwrap();
        writeln!(temp_file, "assign alice admin").unwrap();
        writeln!(temp_file, "role incomplete topic sensors/+").unwrap();
        writeln!(temp_file, "assign alice").unwrap();
        writeln!(temp_file, "assign").unwrap();
        writeln!(temp_file, "user alice topic").unwrap();
        writeln!(temp_file, "role").unwrap();
        temp_file.flush().unwrap();

        let acl = AclManager::from_file(temp_file.path()).await.unwrap();

        assert_eq!(acl.rule_count().await, 1);
        assert_eq!(acl.role_count().await, 1);
        assert_eq!(acl.get_user_roles("alice").await.len(), 1);
    }

    #[tokio::test]
    async fn test_file_parsing_duplicate_assignments() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "role admin topic admin/# permission readwrite").unwrap();
        writeln!(temp_file, "assign alice admin").unwrap();
        writeln!(temp_file, "assign alice admin").unwrap();
        writeln!(temp_file, "assign alice admin").unwrap();
        temp_file.flush().unwrap();

        let acl = AclManager::from_file(temp_file.path()).await.unwrap();

        let alice_roles = acl.get_user_roles("alice").await;
        assert_eq!(alice_roles.len(), 1);
        assert_eq!(alice_roles[0], "admin");
    }

    #[tokio::test]
    async fn test_concurrent_permission_checks_during_modifications() {
        let acl = Arc::new(AclManager::new());

        acl.add_role_rule("reader", "data/#".to_string(), Permission::Read)
            .await
            .unwrap();
        acl.assign_role("alice", "reader").await.unwrap();

        let mut handles = vec![];

        for i in 0..10 {
            let acl_clone = Arc::clone(&acl);
            let handle = tokio::spawn(async move {
                if i % 2 == 0 {
                    acl_clone.check_subscribe(Some("alice"), "data/file").await
                } else {
                    acl_clone.check_publish(Some("alice"), "data/file").await
                }
            });
            handles.push(handle);
        }

        let acl_clone = Arc::clone(&acl);
        let modify_handle1 = tokio::spawn(async move {
            acl_clone
                .add_role_rule("reader", "data/public/#".to_string(), Permission::ReadWrite)
                .await
                .unwrap();
        });

        let acl_clone = Arc::clone(&acl);
        let modify_handle2 = tokio::spawn(async move {
            acl_clone.assign_role("bob", "reader").await.unwrap();
        });

        for handle in handles {
            assert!(handle.await.is_ok());
        }
        assert!(modify_handle1.await.is_ok());
        assert!(modify_handle2.await.is_ok());
    }

    #[tokio::test]
    async fn test_role_rule_removal_edge_cases() {
        let acl = AclManager::new();

        acl.add_role_rule("test-role", "topic1".to_string(), Permission::Read)
            .await
            .unwrap();
        acl.add_role_rule("test-role", "topic2".to_string(), Permission::Write)
            .await
            .unwrap();

        assert!(acl.remove_role_rule("test-role", "topic1").await);
        assert!(!acl.remove_role_rule("test-role", "topic1").await);
        assert!(!acl.remove_role_rule("nonexistent-role", "topic2").await);

        let role = acl.get_role("test-role").await.unwrap();
        assert_eq!(role.rules.len(), 1);
    }

    #[tokio::test]
    async fn test_anonymous_user_with_direct_rule() {
        let acl = AclManager::new();

        acl.add_rule(AclRule::new(
            "anonymous".to_string(),
            "public/#".to_string(),
            Permission::Read,
        ))
        .await;

        assert!(acl.check_subscribe(None, "public/messages").await);
        assert!(!acl.check_publish(None, "public/messages").await);
        assert!(!acl.check_subscribe(None, "private/data").await);
    }

    #[tokio::test]
    async fn test_readwrite_permission_vs_separate_permissions() {
        let acl = AclManager::new();

        acl.add_role_rule("rw-role", "data/#".to_string(), Permission::ReadWrite)
            .await
            .unwrap();
        acl.add_role_rule("split-role", "split/#".to_string(), Permission::Read)
            .await
            .unwrap();
        acl.add_role_rule("split-role", "split/#".to_string(), Permission::Write)
            .await
            .unwrap();

        acl.assign_role("alice", "rw-role").await.unwrap();
        acl.assign_role("bob", "split-role").await.unwrap();

        assert!(acl.check_publish(Some("alice"), "data/file").await);
        assert!(acl.check_subscribe(Some("alice"), "data/file").await);

        assert!(acl.check_publish(Some("bob"), "split/file").await);
        assert!(acl.check_subscribe(Some("bob"), "split/file").await);
    }

    #[tokio::test]
    async fn test_role_deny_multiple_matching_rules() {
        let acl = AclManager::new();

        acl.add_role_rule("mixed", "data/#".to_string(), Permission::ReadWrite)
            .await
            .unwrap();
        acl.add_role_rule("mixed", "data/secret/#".to_string(), Permission::Deny)
            .await
            .unwrap();

        acl.assign_role("alice", "mixed").await.unwrap();

        assert!(acl.check_publish(Some("alice"), "data/public").await);
        assert!(!acl.check_publish(Some("alice"), "data/secret/file").await);
        assert!(!acl.check_subscribe(Some("alice"), "data/secret/file").await);
    }

    #[tokio::test]
    async fn test_unassign_role_from_user_with_multiple_roles() {
        let acl = AclManager::new();

        acl.add_role("role1".to_string()).await;
        acl.add_role("role2".to_string()).await;
        acl.add_role("role3".to_string()).await;

        acl.assign_role("alice", "role1").await.unwrap();
        acl.assign_role("alice", "role2").await.unwrap();
        acl.assign_role("alice", "role3").await.unwrap();

        assert_eq!(acl.get_user_roles("alice").await.len(), 3);

        assert!(acl.unassign_role("alice", "role2").await);
        assert_eq!(acl.get_user_roles("alice").await.len(), 2);

        let remaining_roles = acl.get_user_roles("alice").await;
        assert!(remaining_roles.contains(&"role1".to_string()));
        assert!(remaining_roles.contains(&"role3".to_string()));
        assert!(!remaining_roles.contains(&"role2".to_string()));
    }

    #[tokio::test]
    async fn test_get_role_users_multiple_assignments() {
        let acl = AclManager::new();

        acl.add_role("popular-role".to_string()).await;
        acl.assign_role("alice", "popular-role").await.unwrap();
        acl.assign_role("bob", "popular-role").await.unwrap();
        acl.assign_role("charlie", "popular-role").await.unwrap();

        let users = acl.get_role_users("popular-role").await;
        assert_eq!(users.len(), 3);
        assert!(users.contains(&"alice".to_string()));
        assert!(users.contains(&"bob".to_string()));
        assert!(users.contains(&"charlie".to_string()));

        let empty_users = acl.get_role_users("nonexistent-role").await;
        assert_eq!(empty_users.len(), 0);
    }

    #[tokio::test]
    async fn test_permission_priority_complex_scenario() {
        let acl = AclManager::new();

        acl.add_rule(AclRule::new(
            "*".to_string(),
            "#".to_string(),
            Permission::ReadWrite,
        ))
        .await;

        acl.add_role_rule("restrictive", "admin/#".to_string(), Permission::Deny)
            .await
            .unwrap();
        acl.add_role_rule(
            "permissive",
            "admin/logs/#".to_string(),
            Permission::ReadWrite,
        )
        .await
        .unwrap();

        acl.assign_role("alice", "restrictive").await.unwrap();
        acl.assign_role("alice", "permissive").await.unwrap();

        assert!(acl.check_publish(Some("alice"), "normal/topic").await);
        assert!(!acl.check_publish(Some("alice"), "admin/users").await);
        assert!(!acl.check_publish(Some("alice"), "admin/logs/access").await);

        acl.add_rule(AclRule::new(
            "alice".to_string(),
            "admin/logs/access".to_string(),
            Permission::ReadWrite,
        ))
        .await;

        assert!(acl.check_publish(Some("alice"), "admin/logs/access").await);
        assert!(!acl.check_publish(Some("alice"), "admin/logs/error").await);
    }
}
