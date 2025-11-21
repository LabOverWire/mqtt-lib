use anyhow::{bail, Context, Result};
use argon2::password_hash::{PasswordHasher, Salt, SaltString};
use argon2::Argon2;
use clap::Args;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

#[derive(Args)]
pub struct PasswdCommand {
    #[arg(help = "Username to add/update/delete")]
    pub username: String,

    #[arg(help = "Password file path")]
    pub file: Option<PathBuf>,

    #[arg(long, short, help = "Create new password file (overwrites if exists)")]
    pub create: bool,

    #[arg(
        long,
        short,
        help = "Batch mode - password provided on command line (WARNING: visible in process list)"
    )]
    pub batch: Option<String>,

    #[arg(
        long,
        short = 'D',
        help = "Delete the specified user",
        conflicts_with = "batch"
    )]
    pub delete: bool,

    #[arg(
        long,
        short = 'n',
        help = "Output hash to stdout instead of file",
        conflicts_with_all = &["file", "create", "delete"]
    )]
    pub stdout: bool,
}

pub fn execute(cmd: PasswdCommand) -> Result<()> {
    if cmd.username.contains(':') {
        bail!("Username cannot contain ':' character");
    }

    if cmd.stdout {
        return handle_stdout_mode(&cmd);
    }

    let file_path = cmd
        .file
        .as_ref()
        .context("Password file path required (use -n for stdout mode)")?;

    if cmd.delete {
        return handle_delete(&cmd, file_path);
    }

    handle_add_or_update(&cmd, file_path)
}

fn hash_password(password: &str) -> Result<String> {
    let mut bytes = [0u8; Salt::RECOMMENDED_LENGTH];
    getrandom::fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("Failed to generate random salt: {e}"))?;
    let salt = SaltString::encode_b64(&bytes)
        .map_err(|e| anyhow::anyhow!("Failed to encode salt: {e}"))?;
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| anyhow::anyhow!("Failed to hash password: {e}"))
}

fn handle_stdout_mode(cmd: &PasswdCommand) -> Result<()> {
    let password = get_password(cmd)?;
    let hash = hash_password(&password)?;

    println!("{}:{}", cmd.username, hash);
    Ok(())
}

fn handle_delete(cmd: &PasswdCommand, file_path: &PathBuf) -> Result<()> {
    if !file_path.exists() {
        bail!("Password file does not exist: {}", file_path.display());
    }

    let mut users = read_password_file(file_path)?;

    if users.remove(&cmd.username).is_none() {
        bail!("User '{}' not found in password file", cmd.username);
    }

    write_password_file(file_path, &users)?;
    println!("Deleted user: {}", cmd.username);
    Ok(())
}

fn handle_add_or_update(cmd: &PasswdCommand, file_path: &PathBuf) -> Result<()> {
    let mut users = if cmd.create || !file_path.exists() {
        HashMap::new()
    } else {
        read_password_file(file_path)?
    };

    let password = get_password(cmd)?;
    let hash = hash_password(&password)?;

    let action = if users.contains_key(&cmd.username) {
        "Updated"
    } else {
        "Added"
    };

    users.insert(cmd.username.clone(), hash);
    write_password_file(file_path, &users)?;

    println!("{} user: {}", action, cmd.username);
    Ok(())
}

fn get_password(cmd: &PasswdCommand) -> Result<String> {
    if let Some(ref password) = cmd.batch {
        if !cmd.stdout {
            eprintln!("Warning: Using -b is insecure on multi-user systems");
        }
        return Ok(password.clone());
    }

    let password = rpassword::prompt_password("Password: ").context("Failed to read password")?;

    if password.is_empty() {
        bail!("Password cannot be empty");
    }

    let confirm = rpassword::prompt_password("Confirm password: ")
        .context("Failed to read password confirmation")?;

    if password != confirm {
        bail!("Passwords do not match");
    }

    Ok(password)
}

fn read_password_file(path: &PathBuf) -> Result<HashMap<String, String>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read password file: {}", path.display()))?;

    let mut users = HashMap::new();

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, ':').collect();
        if parts.len() != 2 {
            eprintln!("Warning: Invalid format at line {}: {}", line_num + 1, line);
            continue;
        }

        let username = parts[0].trim().to_string();
        let hash = parts[1].trim().to_string();

        if username.is_empty() {
            eprintln!("Warning: Empty username at line {}", line_num + 1);
            continue;
        }

        users.insert(username, hash);
    }

    Ok(users)
}

fn write_password_file(path: &PathBuf, users: &HashMap<String, String>) -> Result<()> {
    let mut content = String::new();

    let mut usernames: Vec<&String> = users.keys().collect();
    usernames.sort();

    for username in usernames {
        if let Some(hash) = users.get(username) {
            content.push_str(&format!("{username}:{hash}\n"));
        }
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("Failed to open password file: {}", path.display()))?;

    file.write_all(content.as_bytes())
        .with_context(|| format!("Failed to write password file: {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("Failed to set file permissions: {}", path.display()))?;
    }

    Ok(())
}
