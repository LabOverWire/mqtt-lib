#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::large_futures)]

use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser)]
#[command(name = "mqttv5")]
#[command(about = "Superior MQTT v5.0 CLI - unified client and broker tool")]
#[command(version)]
#[command(
    long_about = "A unified CLI tool for MQTT v5.0 with superior input ergonomics and smart defaults."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(long, short, global = true)]
    verbose: bool,

    /// Enable debug logging
    #[arg(long, global = true)]
    debug: bool,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Publish messages to MQTT topics
    Pub(commands::pub_cmd::PubCommand),
    /// Subscribe to MQTT topics
    Sub(commands::sub_cmd::SubCommand),
    /// Start MQTT broker
    Broker(commands::broker_cmd::BrokerCommand),
    /// Run performance benchmarks against a broker
    Bench(commands::bench_cmd::BenchCommand),
    /// Manage ACL file for broker authorization
    Acl(commands::acl_cmd::AclCommand),
    /// Manage password file for broker authentication
    Passwd(commands::passwd_cmd::PasswdCommand),
    /// Manage SCRAM credentials file for SCRAM-SHA-256 authentication
    Scram(commands::scram_cmd::ScramCommand),
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize rustls crypto provider for TLS support
    let _ = rustls::crypto::ring::default_provider().install_default();

    let Cli {
        command,
        verbose,
        debug,
    } = Cli::parse();

    match command {
        Commands::Pub(cmd) => commands::pub_cmd::execute(cmd, verbose, debug).await,
        Commands::Sub(cmd) => commands::sub_cmd::execute(cmd, verbose, debug).await,
        Commands::Broker(cmd) => commands::broker_cmd::execute(cmd, verbose, debug).await,
        Commands::Bench(cmd) => commands::bench_cmd::execute(cmd, verbose, debug).await,
        Commands::Acl(cmd) => {
            init_basic_tracing(verbose, debug);
            commands::acl_cmd::execute(cmd).await
        }
        Commands::Passwd(cmd) => {
            init_basic_tracing(verbose, debug);
            commands::passwd_cmd::execute(&cmd)
        }
        Commands::Scram(cmd) => {
            init_basic_tracing(verbose, debug);
            commands::scram_cmd::execute(&cmd)
        }
    }
}

pub fn init_basic_tracing(verbose: bool, debug: bool) {
    if std::env::var("RUST_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_target(false)
            .without_time()
            .try_init();
    } else {
        let log_level = if debug {
            tracing::Level::DEBUG
        } else if verbose {
            tracing::Level::INFO
        } else {
            tracing::Level::ERROR
        };

        let _ = tracing_subscriber::fmt()
            .with_max_level(log_level)
            .with_target(false)
            .without_time()
            .try_init();
    }
}
