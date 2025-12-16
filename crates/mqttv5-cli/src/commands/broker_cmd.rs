use anyhow::{Context, Result};
use clap::{ArgAction, Args};
use dialoguer::Confirm;
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

    /// Allow anonymous access (no authentication required)
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub allow_anonymous: Option<bool>,

    /// Password file path (format: username:password per line)
    #[arg(long)]
    pub auth_password_file: Option<PathBuf>,

    /// ACL file path (format: `user <username> topic <pattern> permission <type>` per line)
    #[arg(long)]
    pub acl_file: Option<PathBuf>,

    /// Authentication method: password, scram, jwt (default: password if auth file provided)
    #[arg(long, value_parser = ["password", "scram", "jwt"])]
    pub auth_method: Option<String>,

    /// SCRAM credentials file path (format: username:salt:iterations:stored_key:server_key)
    #[arg(long)]
    pub scram_file: Option<PathBuf>,

    /// JWT algorithm: hs256, rs256, es256
    #[arg(long, value_parser = ["hs256", "rs256", "es256"])]
    pub jwt_algorithm: Option<String>,

    /// JWT secret file (for HS256) or public key file (for RS256/ES256)
    #[arg(long)]
    pub jwt_key_file: Option<PathBuf>,

    /// JWT required issuer
    #[arg(long)]
    pub jwt_issuer: Option<String>,

    /// JWT required audience
    #[arg(long)]
    pub jwt_audience: Option<String>,

    /// JWT clock skew tolerance in seconds (default: 60)
    #[arg(long, default_value = "60")]
    pub jwt_clock_skew: u64,

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

    /// QUIC bind address - can be specified multiple times (requires --tls-cert and --tls-key)
    #[arg(long, action = ArgAction::Append)]
    pub quic_host: Vec<String>,

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

fn resolve_auth_settings(cmd: &BrokerCommand) -> Result<bool> {
    if let Some(allow_anon) = cmd.allow_anonymous {
        return Ok(allow_anon);
    }

    if cmd.auth_password_file.is_some() {
        info!("Password file provided, anonymous access disabled by default");
        return Ok(false);
    }

    if cmd.scram_file.is_some() {
        info!("SCRAM credentials file provided, anonymous access disabled by default");
        return Ok(false);
    }

    if cmd.jwt_key_file.is_some() {
        info!("JWT key file provided, anonymous access disabled by default");
        return Ok(false);
    }

    if cmd.non_interactive {
        anyhow::bail!(
            "No authentication configured.\n\
             Use one of:\n  \
             --allow-anonymous           Allow connections without credentials\n  \
             --auth-password-file <path> Require password authentication\n  \
             --scram-file <path>         Require SCRAM-SHA-256 authentication\n  \
             --jwt-key-file <path>       Require JWT authentication"
        );
    }

    eprintln!("⚠️  No authentication configured.");
    let allow_anon = Confirm::new()
        .with_prompt("Allow anonymous connections? (not recommended for production)")
        .default(false)
        .interact()
        .context("Failed to get authentication choice")?;

    Ok(allow_anon)
}

async fn create_interactive_config(cmd: &mut BrokerCommand) -> Result<BrokerConfig> {
    use mqtt5::broker::config::{
        AuthConfig, AuthMethod, JwtAlgorithm, JwtConfig, QuicConfig, StorageConfig, TlsConfig,
        WebSocketConfig,
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

    let allow_anonymous = resolve_auth_settings(cmd)?;

    let auth_method = match cmd.auth_method.as_deref() {
        Some("scram") => {
            let Some(scram_file) = &cmd.scram_file else {
                anyhow::bail!("--scram-file is required when using --auth-method scram");
            };
            if !scram_file.exists() {
                anyhow::bail!("SCRAM credentials file not found: {}", scram_file.display());
            }
            AuthMethod::ScramSha256
        }
        Some("jwt") => {
            let Some(jwt_key_file) = &cmd.jwt_key_file else {
                anyhow::bail!("--jwt-key-file is required when using --auth-method jwt");
            };
            if !jwt_key_file.exists() {
                anyhow::bail!("JWT key file not found: {}", jwt_key_file.display());
            }
            if cmd.jwt_algorithm.is_none() {
                anyhow::bail!("--jwt-algorithm is required when using --auth-method jwt");
            }
            AuthMethod::Jwt
        }
        Some("password") | None => {
            if cmd.auth_password_file.is_some() {
                AuthMethod::Password
            } else {
                AuthMethod::None
            }
        }
        Some(other) => anyhow::bail!("Unknown auth method: {other}"),
    };

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

    let jwt_config = if auth_method == AuthMethod::Jwt {
        let algorithm = match cmd.jwt_algorithm.as_deref() {
            Some("hs256") => JwtAlgorithm::HS256,
            Some("rs256") => JwtAlgorithm::RS256,
            Some("es256") => JwtAlgorithm::ES256,
            _ => anyhow::bail!("Invalid JWT algorithm"),
        };
        let mut jwt_cfg = JwtConfig::new(algorithm, cmd.jwt_key_file.clone().unwrap());
        jwt_cfg.clock_skew_secs = cmd.jwt_clock_skew;
        if let Some(ref issuer) = cmd.jwt_issuer {
            jwt_cfg.issuer = Some(issuer.clone());
        }
        if let Some(ref audience) = cmd.jwt_audience {
            jwt_cfg.audience = Some(audience.clone());
        }
        Some(jwt_cfg)
    } else {
        None
    };

    let auth_config = AuthConfig {
        allow_anonymous,
        password_file: cmd.auth_password_file.clone(),
        acl_file: cmd.acl_file.clone(),
        auth_method,
        auth_data: None,
        scram_file: cmd.scram_file.clone(),
        jwt_config,
    };
    config = config.with_auth(auth_config);

    match auth_method {
        AuthMethod::None if allow_anonymous => {
            info!("Anonymous access enabled - clients can connect without credentials");
        }
        AuthMethod::Password => {
            info!(
                "Password authentication enabled (file: {:?})",
                cmd.auth_password_file.as_ref().unwrap()
            );
        }
        AuthMethod::ScramSha256 => {
            info!(
                "SCRAM-SHA-256 authentication enabled (file: {:?})",
                cmd.scram_file.as_ref().unwrap()
            );
        }
        AuthMethod::Jwt => {
            info!(
                "JWT authentication enabled (algorithm: {:?}, key: {:?})",
                cmd.jwt_algorithm.as_ref().unwrap(),
                cmd.jwt_key_file.as_ref().unwrap()
            );
        }
        _ => {}
    }

    if let Some(acl_file) = &cmd.acl_file {
        info!("ACL authorization enabled (file: {:?})", acl_file);
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

    // Configure QUIC
    if !cmd.quic_host.is_empty() {
        if let (Some(cert), Some(key)) = (&cmd.tls_cert, &cmd.tls_key) {
            if !cert.exists() {
                anyhow::bail!("TLS certificate file not found: {}", cert.display());
            }
            if !key.exists() {
                anyhow::bail!("TLS key file not found: {}", key.display());
            }

            let quic_addrs: Result<Vec<std::net::SocketAddr>> = cmd
                .quic_host
                .iter()
                .map(|h| {
                    h.parse()
                        .with_context(|| format!("Invalid QUIC bind address: {h}"))
                })
                .collect();

            let mut quic_config =
                QuicConfig::new(cert.clone(), key.clone()).with_bind_addresses(quic_addrs?);

            if let Some(ca_cert) = &cmd.tls_ca_cert {
                if !ca_cert.exists() {
                    anyhow::bail!("TLS CA certificate file not found: {}", ca_cert.display());
                }
                quic_config = quic_config
                    .with_ca_file(ca_cert.clone())
                    .with_require_client_cert(cmd.tls_require_client_cert);
            }

            config = config.with_quic(quic_config);
            info!("QUIC enabled on {:?}", cmd.quic_host);
        } else {
            anyhow::bail!("Both --tls-cert and --tls-key must be provided when using --quic-host");
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
