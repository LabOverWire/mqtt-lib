#![allow(clippy::redundant_else)]

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use mqtt5::broker::acl::AclManager;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

#[derive(Args)]
pub struct AclCommand {
    #[command(subcommand)]
    pub action: AclAction,
}

#[derive(Subcommand)]
pub enum AclAction {
    Add {
        #[arg(help = "Username (or * for wildcard)")]
        username: String,

        #[arg(help = "Topic pattern (supports + and # wildcards)")]
        topic: String,

        #[arg(help = "Permission: read, write, readwrite, or deny")]
        permission: String,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },

    Remove {
        #[arg(help = "Username to remove rules for")]
        username: String,

        #[arg(help = "Topic pattern to remove (optional, removes all if not specified)")]
        topic: Option<String>,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },

    List {
        #[arg(help = "Username to list rules for (optional, lists all if not specified)")]
        username: Option<String>,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },

    Check {
        #[arg(help = "Username to check")]
        username: String,

        #[arg(help = "Topic to check")]
        topic: String,

        #[arg(help = "Action: read or write")]
        action: String,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },

    #[command(name = "role-add")]
    RoleAdd {
        #[arg(help = "Role name")]
        role_name: String,

        #[arg(help = "Topic pattern (supports + and # wildcards)")]
        topic: String,

        #[arg(help = "Permission: read, write, readwrite, or deny")]
        permission: String,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },

    #[command(name = "role-remove")]
    RoleRemove {
        #[arg(help = "Role name")]
        role_name: String,

        #[arg(help = "Topic pattern to remove (optional, removes entire role if not specified)")]
        topic: Option<String>,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },

    #[command(name = "role-list")]
    RoleList {
        #[arg(help = "Role name to show (optional, lists all roles if not specified)")]
        role_name: Option<String>,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },

    Assign {
        #[arg(help = "Username to assign role to")]
        username: String,

        #[arg(help = "Role name to assign")]
        role_name: String,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },

    Unassign {
        #[arg(help = "Username to remove role from")]
        username: String,

        #[arg(help = "Role name to remove")]
        role_name: String,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },

    #[command(name = "user-roles")]
    UserRoles {
        #[arg(help = "Username to list roles for")]
        username: String,

        #[arg(long, short, help = "ACL file path")]
        file: PathBuf,
    },
}

pub async fn execute(cmd: AclCommand) -> Result<()> {
    match cmd.action {
        AclAction::Add {
            username,
            topic,
            permission,
            file,
        } => handle_add(&username, &topic, &permission, &file).await,
        AclAction::Remove {
            username,
            topic,
            file,
        } => handle_remove(&username, topic.as_deref(), &file).await,
        AclAction::List { username, file } => handle_list(username.as_deref(), &file).await,
        AclAction::Check {
            username,
            topic,
            action,
            file,
        } => handle_check(&username, &topic, &action, &file).await,
        AclAction::RoleAdd {
            role_name,
            topic,
            permission,
            file,
        } => handle_role_add(&role_name, &topic, &permission, &file).await,
        AclAction::RoleRemove {
            role_name,
            topic,
            file,
        } => handle_role_remove(&role_name, topic.as_deref(), &file).await,
        AclAction::RoleList { role_name, file } => {
            handle_role_list(role_name.as_deref(), &file).await
        }
        AclAction::Assign {
            username,
            role_name,
            file,
        } => handle_assign(&username, &role_name, &file).await,
        AclAction::Unassign {
            username,
            role_name,
            file,
        } => handle_unassign(&username, &role_name, &file).await,
        AclAction::UserRoles { username, file } => handle_user_roles(&username, &file).await,
    }
}

async fn handle_add(
    username: &str,
    topic: &str,
    permission: &str,
    file_path: &PathBuf,
) -> Result<()> {
    let valid_permissions = ["read", "write", "readwrite", "deny"];
    if !valid_permissions.contains(&permission) {
        bail!(
            "Invalid permission '{}'. Must be one of: {}",
            permission,
            valid_permissions.join(", ")
        );
    }

    if username.contains(char::is_whitespace) {
        bail!("Username cannot contain whitespace");
    }

    if topic.contains(char::is_whitespace) {
        bail!("Topic pattern cannot contain whitespace");
    }

    let mut rules = if file_path.exists() {
        read_acl_file(file_path).await?
    } else {
        Vec::new()
    };

    let rule_line = format!("user {username} topic {topic} permission {permission}");

    if rules.iter().any(|r| r == &rule_line) {
        println!("Rule already exists: {rule_line}");
        return Ok(());
    }

    rules.push(rule_line.clone());
    write_acl_file(file_path, &rules).await?;

    println!("Added ACL rule: {rule_line}");
    Ok(())
}

async fn handle_remove(username: &str, topic: Option<&str>, file_path: &PathBuf) -> Result<()> {
    if !file_path.exists() {
        bail!("ACL file does not exist: {}", file_path.display());
    }

    let mut rules = read_acl_file(file_path).await?;
    let original_count = rules.len();

    rules.retain(|rule| {
        let parts: Vec<&str> = rule.split_whitespace().collect();
        if parts.len() != 6 || parts[0] != "user" || parts[2] != "topic" || parts[4] != "permission"
        {
            return true;
        }

        let rule_username = parts[1];
        let rule_topic = parts[3];

        if rule_username != username {
            return true;
        }

        if let Some(topic_filter) = topic {
            rule_topic != topic_filter
        } else {
            false
        }
    });

    let removed_count = original_count - rules.len();

    if removed_count == 0 {
        if let Some(topic_filter) = topic {
            bail!("No rules found for user '{username}' with topic '{topic_filter}'");
        } else {
            bail!("No rules found for user '{username}'");
        }
    }

    write_acl_file(file_path, &rules).await?;

    if let Some(topic_filter) = topic {
        println!(
            "Removed {removed_count} rule(s) for user '{username}' with topic '{topic_filter}'"
        );
    } else {
        println!("Removed {removed_count} rule(s) for user '{username}'");
    }

    Ok(())
}

async fn handle_list(username: Option<&str>, file_path: &PathBuf) -> Result<()> {
    if !file_path.exists() {
        bail!("ACL file does not exist: {}", file_path.display());
    }

    let rules = read_acl_file(file_path).await?;

    if rules.is_empty() {
        println!("No ACL rules found");
        return Ok(());
    }

    let filtered_rules: Vec<&String> = if let Some(user_filter) = username {
        rules
            .iter()
            .filter(|rule| {
                let parts: Vec<&str> = rule.split_whitespace().collect();
                parts.len() >= 2 && parts[0] == "user" && parts[1] == user_filter
            })
            .collect()
    } else {
        rules.iter().collect()
    };

    if filtered_rules.is_empty() {
        if let Some(user_filter) = username {
            println!("No ACL rules found for user '{user_filter}'");
        } else {
            println!("No ACL rules found");
        }
        return Ok(());
    }

    if let Some(user_filter) = username {
        println!("ACL rules for user '{user_filter}':");
    } else {
        println!("All ACL rules:");
    }

    for rule in &filtered_rules {
        println!("  {rule}");
    }

    println!("\nTotal: {} rule(s)", filtered_rules.len());
    Ok(())
}

async fn handle_check(
    username: &str,
    topic: &str,
    action: &str,
    file_path: &PathBuf,
) -> Result<()> {
    if action != "read" && action != "write" {
        bail!("Action must be 'read' or 'write', got: {action}");
    }

    if !file_path.exists() {
        bail!("ACL file does not exist: {}", file_path.display());
    }

    let acl_manager = AclManager::from_file(file_path).await?;

    let allowed = if action == "read" {
        acl_manager.check_subscribe(Some(username), topic).await
    } else {
        acl_manager.check_publish(Some(username), topic).await
    };

    if allowed {
        println!("✓ User '{username}' is ALLOWED to {action} topic '{topic}'");
    } else {
        println!("✗ User '{username}' is DENIED to {action} topic '{topic}'");
    }

    Ok(())
}

async fn handle_role_add(
    role_name: &str,
    topic: &str,
    permission: &str,
    file_path: &PathBuf,
) -> Result<()> {
    let valid_permissions = ["read", "write", "readwrite", "deny"];
    if !valid_permissions.contains(&permission) {
        bail!(
            "Invalid permission '{}'. Must be one of: {}",
            permission,
            valid_permissions.join(", ")
        );
    }

    if role_name.is_empty() || role_name.contains(char::is_whitespace) {
        bail!("Role name cannot be empty or contain whitespace");
    }

    if topic.contains(char::is_whitespace) {
        bail!("Topic pattern cannot contain whitespace");
    }

    let mut rules = if file_path.exists() {
        read_acl_file(file_path).await?
    } else {
        Vec::new()
    };

    let rule_line = format!("role {role_name} topic {topic} permission {permission}");

    if rules.iter().any(|r| r == &rule_line) {
        println!("Role rule already exists: {rule_line}");
        return Ok(());
    }

    rules.push(rule_line.clone());
    write_acl_file(file_path, &rules).await?;

    println!("Added role rule: {rule_line}");
    Ok(())
}

async fn handle_role_remove(
    role_name: &str,
    topic: Option<&str>,
    file_path: &PathBuf,
) -> Result<()> {
    if !file_path.exists() {
        bail!("ACL file does not exist: {}", file_path.display());
    }

    let mut rules = read_acl_file(file_path).await?;
    let original_count = rules.len();

    rules.retain(|rule| {
        let parts: Vec<&str> = rule.split_whitespace().collect();
        if parts.len() != 6 || parts[0] != "role" || parts[2] != "topic" || parts[4] != "permission"
        {
            return true;
        }

        let rule_role = parts[1];
        let rule_topic = parts[3];

        if rule_role != role_name {
            return true;
        }

        if let Some(topic_filter) = topic {
            rule_topic != topic_filter
        } else {
            false
        }
    });

    let removed_count = original_count - rules.len();

    if removed_count == 0 {
        if let Some(topic_filter) = topic {
            bail!("No rules found for role '{role_name}' with topic '{topic_filter}'");
        } else {
            bail!("No rules found for role '{role_name}'");
        }
    }

    write_acl_file(file_path, &rules).await?;

    if let Some(topic_filter) = topic {
        println!(
            "Removed {removed_count} rule(s) from role '{role_name}' with topic '{topic_filter}'"
        );
    } else {
        println!("Removed role '{role_name}' ({removed_count} rule(s))");
    }

    Ok(())
}

async fn handle_role_list(role_name: Option<&str>, file_path: &PathBuf) -> Result<()> {
    if !file_path.exists() {
        bail!("ACL file does not exist: {}", file_path.display());
    }

    let rules = read_acl_file(file_path).await?;

    let role_rules: Vec<&String> = rules
        .iter()
        .filter(|rule| {
            let parts: Vec<&str> = rule.split_whitespace().collect();
            if parts.len() < 2 || parts[0] != "role" {
                return false;
            }
            if let Some(filter) = role_name {
                parts[1] == filter
            } else {
                true
            }
        })
        .collect();

    if role_rules.is_empty() {
        if let Some(filter) = role_name {
            println!("No rules found for role '{filter}'");
        } else {
            println!("No roles defined");
        }
        return Ok(());
    }

    if let Some(filter) = role_name {
        println!("Rules for role '{filter}':");
    } else {
        println!("All role definitions:");
    }

    for rule in &role_rules {
        println!("  {rule}");
    }

    println!("\nTotal: {} rule(s)", role_rules.len());
    Ok(())
}

async fn handle_assign(username: &str, role_name: &str, file_path: &PathBuf) -> Result<()> {
    if username.is_empty() || username.contains(char::is_whitespace) {
        bail!("Username cannot be empty or contain whitespace");
    }

    if role_name.is_empty() || role_name.contains(char::is_whitespace) {
        bail!("Role name cannot be empty or contain whitespace");
    }

    let mut rules = if file_path.exists() {
        read_acl_file(file_path).await?
    } else {
        Vec::new()
    };

    let assign_line = format!("assign {username} {role_name}");

    if rules.iter().any(|r| r == &assign_line) {
        println!("Assignment already exists: {assign_line}");
        return Ok(());
    }

    rules.push(assign_line.clone());
    write_acl_file(file_path, &rules).await?;

    println!("Assigned role '{role_name}' to user '{username}'");
    Ok(())
}

async fn handle_unassign(username: &str, role_name: &str, file_path: &PathBuf) -> Result<()> {
    if !file_path.exists() {
        bail!("ACL file does not exist: {}", file_path.display());
    }

    let mut rules = read_acl_file(file_path).await?;
    let original_count = rules.len();

    let assign_line = format!("assign {username} {role_name}");
    rules.retain(|rule| rule != &assign_line);

    if rules.len() == original_count {
        bail!("No assignment found for user '{username}' with role '{role_name}'");
    }

    write_acl_file(file_path, &rules).await?;

    println!("Removed role '{role_name}' from user '{username}'");
    Ok(())
}

async fn handle_user_roles(username: &str, file_path: &PathBuf) -> Result<()> {
    if !file_path.exists() {
        bail!("ACL file does not exist: {}", file_path.display());
    }

    let rules = read_acl_file(file_path).await?;

    let user_roles: Vec<&str> = rules
        .iter()
        .filter_map(|rule| {
            let parts: Vec<&str> = rule.split_whitespace().collect();
            if parts.len() == 3 && parts[0] == "assign" && parts[1] == username {
                Some(parts[2])
            } else {
                None
            }
        })
        .collect();

    if user_roles.is_empty() {
        println!("User '{username}' has no assigned roles");
        return Ok(());
    }

    println!("Roles for user '{username}':");
    for role in &user_roles {
        println!("  {role}");
    }

    Ok(())
}

async fn read_acl_file(path: &PathBuf) -> Result<Vec<String>> {
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read ACL file: {}", path.display()))?;

    let mut rules = Vec::new();

    for line in content.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        rules.push(line.to_string());
    }

    Ok(rules)
}

async fn write_acl_file(path: &PathBuf, rules: &[String]) -> Result<()> {
    let mut content = String::new();

    for rule in rules {
        content.push_str(rule);
        content.push('\n');
    }

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .await
        .with_context(|| format!("Failed to open ACL file: {}", path.display()))?;

    file.write_all(content.as_bytes())
        .await
        .with_context(|| format!("Failed to write ACL file: {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(path, permissions)
            .await
            .with_context(|| format!("Failed to set file permissions: {}", path.display()))?;
    }

    Ok(())
}
