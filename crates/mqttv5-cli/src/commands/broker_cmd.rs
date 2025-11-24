use anyhow::{Context, Result};
use clap::{ArgAction, Args};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use std::path::{Path, PathBuf};
use tokio::signal;
use tracing::{debug, info};

#[derive(Args)]
pub struct BrokerCommand {
    /// Configuration file path (JSON format)
    #[arg(long, short)]
    pub config: Option<PathBuf>,

    /// TCP bind address (e.g., `0.0.0.0:1883` `[::]:1883`) - can be specified multiple times
    #[arg(long, short = 'H', action = ArgAction::Append)]
    pub host: Vec<String>,

    /// Maximum number of concurrent clients
    #[arg(long, default_value = "10000")]
    pub max_clients: usize,

    /// Allow anonymous access (no authentication required) - enabled by default
    #[arg(long, default_value_t = true)]
    pub allow_anonymous: bool,

    /// Password file path (format: username:password per line)
    #[arg(long)]
    pub auth_password_file: Option<PathBuf>,

    /// ACL file path (format: `user <username> topic <pattern> permission <type>` per line)
    #[arg(long)]
    pub acl_file: Option<PathBuf>,

    /// TLS certificate file path (PEM format)
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,

    /// TLS private key file path (PEM format)
    #[arg(long)]
    pub tls_key: Option<PathBuf>,

    /// TLS CA certificate file for client verification (PEM format, enables mTLS)
    #[arg(long)]
    pub tls_ca_cert: Option<PathBuf>,

    /// Require client certificates for TLS connections (mutual TLS)
    #[arg(long)]
    pub tls_require_client_cert: bool,

    /// TLS bind address - can be specified multiple times
    #[arg(long, action = ArgAction::Append)]
    pub tls_host: Vec<String>,

    /// WebSocket bind address - can be specified multiple times
    #[arg(long, action = ArgAction::Append)]
    pub ws_host: Vec<String>,

    /// WebSocket TLS bind address - can be specified multiple times
    #[arg(long, action = ArgAction::Append)]
    pub ws_tls_host: Vec<String>,

    /// WebSocket path (e.g., /mqtt)
    #[arg(long, default_value = "/mqtt")]
    pub ws_path: String,

    /// Storage directory for persistent data
    #[arg(long, default_value = "./mqtt_storage")]
    pub storage_dir: PathBuf,

    /// Disable message persistence
    #[arg(long)]
    pub no_persistence: bool,

    /// Session expiry interval in seconds (default: 3600)
    #[arg(long, default_value = "3600")]
    pub session_expiry: u64,

    /// Maximum QoS level supported (0, 1, or 2)
    #[arg(long, default_value = "2")]
    pub max_qos: u8,

    /// Server keep-alive time in seconds (optional)
    #[arg(long)]
    pub keep_alive: Option<u16>,

    /// Response information string sent to clients that request it
    #[arg(long)]
    pub response_information: Option<String>,

    /// Disable retained messages
    #[arg(long)]
    pub no_retain: bool,

    /// Disable wildcard subscriptions
    #[arg(long)]
    pub no_wildcards: bool,

    /// Skip prompts and use defaults
    #[arg(long)]
    pub non_interactive: bool,

    /// OpenTelemetry OTLP endpoint (e.g., http://localhost:4317)
    #[cfg(feature = "opentelemetry")]
    #[arg(long)]
    pub otel_endpoint: Option<String>,

    /// OpenTelemetry service name (default: mqttv5-broker)
    #[cfg(feature = "opentelemetry")]
    #[arg(long, default_value = "mqttv5-broker")]
    pub otel_service_name: String,

    /// OpenTelemetry sampling ratio (0.0-1.0, default: 1.0)
    #[cfg(feature = "opentelemetry")]
    #[arg(long, default_value = "1.0")]
    pub otel_sampling: f64,
}

pub async fn execute(mut cmd: BrokerCommand, verbose: bool, debug: bool) -> Result<()> {
    #[cfg(feature = "opentelemetry")]
    let has_otel = cmd.otel_endpoint.is_some();

    #[cfg(not(feature = "opentelemetry"))]
    let has_otel = false;

    if !has_otel {
        crate::init_basic_tracing(verbose, debug);
    }

    info!("Starting MQTT v5.0 broker...");

    // Create broker configuration
    let config = if let Some(config_path) = &cmd.config {
        debug!("Loading configuration from: {:?}", config_path);
        load_config_from_file(config_path)
            .await
            .with_context(|| format!("Failed to load config from {config_path:?}"))?
    } else {
        // Smart prompting for configuration
        create_interactive_config(&mut cmd).await?
    };

    // Validate configuration
    config
        .validate()
        .context("Configuration validation failed")?;

    // Create and start broker
    info!(
        "Creating broker with bind addresses: {:?}",
        config.bind_addresses
    );
    let mut broker = MqttBroker::with_config(config.clone())
        .await
        .context("Failed to create MQTT broker")?;

    println!("🚀 MQTT v5.0 broker starting...");
    println!(
        "  📡 TCP: {}",
        config
            .bind_addresses
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(ref tls_cfg) = config.tls_config {
        println!(
            "  🔒 TLS: {}",
            tls_cfg
                .bind_addresses
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(ref ws_cfg) = config.websocket_config {
        println!(
            "  🌐 WebSocket: {} (path: {})",
            ws_cfg
                .bind_addresses
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            ws_cfg.path
        );
    }
    if let Some(ref ws_tls_cfg) = config.websocket_tls_config {
        println!(
            "  🔐 WebSocket TLS: {} (path: {})",
            ws_tls_cfg
                .bind_addresses
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            ws_tls_cfg.path
        );
    }
    println!("  👥 Max clients: {}", cmd.max_clients);
    #[cfg(feature = "opentelemetry")]
    if let Some(ref otel_config) = config.opentelemetry_config {
        println!(
            "  📊 OpenTelemetry: {} (service: {})",
            otel_config.otlp_endpoint, otel_config.service_name
        );
    }
    println!("  📝 Press Ctrl+C to stop");

    // Set up signal handling
    let shutdown_signal = async {
        match signal::ctrl_c().await {
            Ok(()) => {
                println!("\n🛑 Received Ctrl+C, shutting down gracefully...");
            }
            Err(err) => {
                tracing::error!("Unable to listen for shutdown signal: {}", err);
            }
        }
    };

    // Run broker with graceful shutdown
    tokio::select! {
        result = broker.run() => {
            match result {
                Ok(()) => {
                    info!("Broker stopped normally");
                }
                Err(e) => {
                    anyhow::bail!("Broker error: {e}");
                }
            }
        }
        _ = shutdown_signal => {
            info!("Shutdown signal received, stopping broker...");
        }
    }

    println!("✓ MQTT broker stopped");
    Ok(())
}

async fn create_interactive_config(cmd: &mut BrokerCommand) -> Result<BrokerConfig> {
    use mqtt5::broker::config::{
        AuthConfig, AuthMethod, StorageConfig, TlsConfig, WebSocketConfig,
    };

    let mut config = BrokerConfig::new();

    // Parse bind addresses
    let bind_addrs: Result<Vec<std::net::SocketAddr>> = if cmd.host.is_empty() {
        Ok(vec![
            "0.0.0.0:1883".parse().unwrap(),
            "[::]:1883".parse().unwrap(),
        ])
    } else {
        cmd.host
            .iter()
            .map(|h| {
                h.parse()
                    .with_context(|| format!("Invalid bind address: {h}"))
            })
            .collect()
    };
    config = config.with_bind_addresses(bind_addrs?);

    // Set basic broker parameters
    config = config.with_max_clients(cmd.max_clients);
    config.session_expiry_interval = std::time::Duration::from_secs(cmd.session_expiry);
    config.maximum_qos = cmd.max_qos;
    config.retain_available = !cmd.no_retain;
    config.wildcard_subscription_available = !cmd.no_wildcards;

    if let Some(keep_alive) = cmd.keep_alive {
        config.server_keep_alive = Some(std::time::Duration::from_secs(keep_alive as u64));
    }

    if let Some(ref response_info) = cmd.response_information {
        config.response_information = Some(response_info.clone());
    }

    // Configure authentication
    if cmd.auth_password_file.is_some() || cmd.acl_file.is_some() {
        if let Some(password_file) = &cmd.auth_password_file {
            if !password_file.exists() {
                anyhow::bail!(
                    "Authentication password file not found: {}",
                    password_file.display()
                );
            }
        }

        if let Some(acl_file) = &cmd.acl_file {
            if !acl_file.exists() {
                anyhow::bail!("ACL file not found: {}", acl_file.display());
            }
        }

        let auth_config = AuthConfig {
            allow_anonymous: cmd.allow_anonymous,
            password_file: cmd.auth_password_file.clone(),
            acl_file: cmd.acl_file.clone(),
            auth_method: if cmd.auth_password_file.is_some() {
                AuthMethod::Password
            } else {
                AuthMethod::None
            },
            auth_data: None,
        };
        config = config.with_auth(auth_config);

        if cmd.auth_password_file.is_some() && cmd.acl_file.is_some() {
            info!(
                "Authentication enabled with password file: {:?} and ACL file: {:?}",
                cmd.auth_password_file, cmd.acl_file
            );
        } else if let Some(password_file) = &cmd.auth_password_file {
            info!(
                "Authentication enabled with password file: {:?}",
                password_file
            );
        } else if let Some(acl_file) = &cmd.acl_file {
            info!("Authorization enabled with ACL file: {:?}", acl_file);
        }
    }

    // Configure TLS
    if let (Some(cert), Some(key)) = (&cmd.tls_cert, &cmd.tls_key) {
        // Check if certificate files exist
        if !cert.exists() {
            anyhow::bail!("TLS certificate file not found: {}", cert.display());
        }
        if !key.exists() {
            anyhow::bail!("TLS key file not found: {}", key.display());
        }

        let tls_addrs: Result<Vec<std::net::SocketAddr>> = if cmd.tls_host.is_empty() {
            Ok(vec![
                "0.0.0.0:8883".parse().unwrap(),
                "[::]:8883".parse().unwrap(),
            ])
        } else {
            cmd.tls_host
                .iter()
                .map(|h| {
                    h.parse()
                        .with_context(|| format!("Invalid TLS bind address: {h}"))
                })
                .collect()
        };

        let mut tls_config =
            TlsConfig::new(cert.clone(), key.clone()).with_bind_addresses(tls_addrs?);

        if let Some(ca_cert) = &cmd.tls_ca_cert {
            if !ca_cert.exists() {
                anyhow::bail!("TLS CA certificate file not found: {}", ca_cert.display());
            }
            tls_config = tls_config
                .with_ca_file(ca_cert.clone())
                .with_require_client_cert(cmd.tls_require_client_cert);
            info!("TLS enabled with mTLS (client certificate verification)");
        } else if cmd.tls_require_client_cert {
            anyhow::bail!("--tls-ca-cert is required when --tls-require-client-cert is set");
        } else {
            info!("TLS enabled");
        }

        config = config.with_tls(tls_config);
    } else if cmd.tls_cert.is_some() || cmd.tls_key.is_some() {
        anyhow::bail!("Both --tls-cert and --tls-key must be provided together");
    } else if cmd.tls_ca_cert.is_some() || cmd.tls_require_client_cert {
        anyhow::bail!("--tls-cert and --tls-key must be provided to use --tls-ca-cert or --tls-require-client-cert");
    }

    // Configure WebSocket
    if !cmd.ws_host.is_empty() {
        let ws_addrs: Result<Vec<std::net::SocketAddr>> = cmd
            .ws_host
            .iter()
            .map(|h| {
                h.parse()
                    .with_context(|| format!("Invalid WebSocket bind address: {h}"))
            })
            .collect();

        let ws_config = WebSocketConfig::default()
            .with_bind_addresses(ws_addrs?)
            .with_path(cmd.ws_path.clone());
        config = config.with_websocket(ws_config);
        info!("WebSocket enabled");
    }

    // Configure WebSocket TLS
    if !cmd.ws_tls_host.is_empty() {
        if let (Some(cert), Some(key)) = (&cmd.tls_cert, &cmd.tls_key) {
            if !cert.exists() {
                anyhow::bail!("TLS certificate file not found: {}", cert.display());
            }
            if !key.exists() {
                anyhow::bail!("TLS key file not found: {}", key.display());
            }

            let ws_tls_addrs: Result<Vec<std::net::SocketAddr>> = cmd
                .ws_tls_host
                .iter()
                .map(|h| {
                    h.parse()
                        .with_context(|| format!("Invalid WebSocket TLS bind address: {h}"))
                })
                .collect();

            let ws_tls_config = WebSocketConfig::default()
                .with_bind_addresses(ws_tls_addrs?)
                .with_path(cmd.ws_path.clone())
                .with_tls(true);
            config = config.with_websocket_tls(ws_tls_config);
            info!("WebSocket TLS enabled");
        } else {
            anyhow::bail!(
                "Both --tls-cert and --tls-key must be provided when using --ws-tls-host"
            );
        }
    }

    // Configure storage
    let storage_config = StorageConfig {
        enable_persistence: !cmd.no_persistence,
        base_dir: cmd.storage_dir.clone(),
        backend: mqtt5::broker::config::StorageBackend::File,
        cleanup_interval: std::time::Duration::from_secs(300), // 5 minutes
    };
    config.storage_config = storage_config;

    #[cfg(feature = "opentelemetry")]
    if let Some(endpoint) = &cmd.otel_endpoint {
        use mqtt5::telemetry::TelemetryConfig;
        let telemetry_config = TelemetryConfig::new(&cmd.otel_service_name)
            .with_endpoint(endpoint)
            .with_sampling_ratio(cmd.otel_sampling);
        config = config.with_opentelemetry(telemetry_config);
        info!(
            "OpenTelemetry enabled: endpoint={}, service={}, sampling={}",
            endpoint, cmd.otel_service_name, cmd.otel_sampling
        );
    }

    Ok(config)
}

async fn load_config_from_file(config_path: &Path) -> Result<BrokerConfig> {
    let contents = std::fs::read_to_string(config_path).context("Failed to read config file")?;

    serde_json::from_str(&contents).context("Failed to parse config file as JSON")
}
