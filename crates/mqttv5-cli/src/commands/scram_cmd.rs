use anyhow::{bail, Context, Result};
use clap::Args;
use mqtt5::broker::auth_mechanisms::{
    generate_scram_credential_line, generate_scram_credential_line_with_iterations,
};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const DEFAULT_ITERATIONS: u32 = 310_000;

#[derive(Args)]
pub struct ScramCommand {
    #[arg(help = "Username to add/update/delete")]
    pub username: String,

    #[arg(help = "SCRAM credentials file path")]
    pub file: Option<PathBuf>,

    #[arg(long, short, help = "Create new SCRAM file (overwrites if exists)")]
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
        help = "Output credentials to stdout instead of file",
        conflicts_with_all = &["file", "create", "delete"]
    )]
    pub stdout: bool,

    #[arg(
        long,
        short = 'i',
        help = "PBKDF2 iteration count (default: 310000)",
        default_value = "310000"
    )]
    pub iterations: u32,
}

pub fn execute(cmd: ScramCommand) -> Result<()> {
    if cmd.username.contains(':') {
        bail!("Username cannot contain ':' character");
    }

    if cmd.iterations < 10000 {
        bail!("Iteration count must be at least 10000 for security");
    }

    if cmd.stdout {
        return handle_stdout_mode(&cmd);
    }

    let file_path = cmd
        .file
        .as_ref()
        .context("SCRAM file path required (use -n for stdout mode)")?;

    if cmd.delete {
        return handle_delete(&cmd, file_path);
    }

    handle_add_or_update(&cmd, file_path)
}

fn handle_stdout_mode(cmd: &ScramCommand) -> Result<()> {
    let password = get_password(cmd)?;
    let line = if cmd.iterations == DEFAULT_ITERATIONS {
        generate_scram_credential_line(&cmd.username, &password)
    } else {
        generate_scram_credential_line_with_iterations(&cmd.username, &password, cmd.iterations)
    }
    .context("Failed to generate SCRAM credentials")?;
    println!("{line}");
    Ok(())
}

fn handle_delete(cmd: &ScramCommand, file_path: &PathBuf) -> Result<()> {
    if !file_path.exists() {
        bail!("SCRAM file does not exist: {}", file_path.display());
    }

    let mut users = read_scram_file(file_path)?;

    if users.remove(&cmd.username).is_none() {
        bail!("User '{}' not found in SCRAM file", cmd.username);
    }

    write_scram_file(file_path, &users)?;
    println!("Deleted user: {}", cmd.username);
    Ok(())
}

fn handle_add_or_update(cmd: &ScramCommand, file_path: &PathBuf) -> Result<()> {
    let mut users = if cmd.create || !file_path.exists() {
        HashMap::new()
    } else {
        read_scram_file(file_path)?
    };

    let password = get_password(cmd)?;
    let line = if cmd.iterations == DEFAULT_ITERATIONS {
        generate_scram_credential_line(&cmd.username, &password)
    } else {
        generate_scram_credential_line_with_iterations(&cmd.username, &password, cmd.iterations)
    }
    .context("Failed to generate SCRAM credentials")?;

    let action = if users.contains_key(&cmd.username) {
        "Updated"
    } else {
        "Added"
    };

    users.insert(cmd.username.clone(), line);
    write_scram_file(file_path, &users)?;

    println!("{} user: {}", action, cmd.username);
    Ok(())
}

fn get_password(cmd: &ScramCommand) -> Result<String> {
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

fn read_scram_file(path: &PathBuf) -> Result<HashMap<String, String>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read SCRAM file: {}", path.display()))?;

    let mut users = HashMap::new();

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() != 5 {
            eprintln!(
                "Warning: Invalid SCRAM format at line {} (expected 5 fields, got {})",
                line_num + 1,
                parts.len()
            );
            continue;
        }

        let username = parts[0].to_string();
        if username.is_empty() {
            eprintln!("Warning: Empty username at line {}", line_num + 1);
            continue;
        }

        users.insert(username, line.to_string());
    }

    Ok(users)
}

fn write_scram_file(path: &PathBuf, users: &HashMap<String, String>) -> Result<()> {
    let mut content = String::new();

    let mut usernames: Vec<&String> = users.keys().collect();
    usernames.sort();

    for username in usernames {
        if let Some(line) = users.get(username) {
            content.push_str(line);
            content.push('\n');
        }
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("Failed to open SCRAM file: {}", path.display()))?;

    file.write_all(content.as_bytes())
        .with_context(|| format!("Failed to write SCRAM file: {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, permissions)
            .with_context(|| format!("Failed to set file permissions: {}", path.display()))?;
    }

    Ok(())
}
