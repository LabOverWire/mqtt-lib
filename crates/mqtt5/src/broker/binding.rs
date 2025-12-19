use std::fmt::Write;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::{info, warn};

#[derive(Debug)]
pub struct BindFailure {
    pub address: SocketAddr,
    pub error: std::io::Error,
}

#[derive(Debug)]
pub struct BindResult<T> {
    pub successful: Vec<T>,
    pub failures: Vec<BindFailure>,
}

impl<T> BindResult<T> {
    pub fn is_empty(&self) -> bool {
        self.successful.is_empty()
    }

    #[allow(dead_code)]
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }
}

pub async fn bind_tcp_addresses(
    addrs: &[SocketAddr],
    transport_name: &str,
) -> BindResult<TcpListener> {
    let mut successful = Vec::new();
    let mut failures = Vec::new();

    for addr in addrs {
        match TcpListener::bind(addr).await {
            Ok(listener) => {
                info!("MQTT broker {} listening on {}", transport_name, addr);
                successful.push(listener);
            }
            Err(e) => {
                warn!("Failed to bind {} on {} ({})", transport_name, addr, e);
                failures.push(BindFailure {
                    address: *addr,
                    error: e,
                });
            }
        }
    }

    BindResult {
        successful,
        failures,
    }
}

pub fn format_binding_error(
    transport_name: &str,
    failures: &[BindFailure],
    attempted_addrs: &[SocketAddr],
) -> String {
    let mut msg = format!(
        "Failed to bind to any {} address. Attempted addresses:\n",
        transport_name
    );

    for failure in failures {
        writeln!(
            msg,
            "  - {}: {} ({})",
            failure.address,
            failure.error,
            error_kind_to_hint(&failure.error)
        )
        .ok();
    }

    if attempted_addrs.len() > failures.len() {
        let successful_count = attempted_addrs.len() - failures.len();
        writeln!(
            msg,
            "\nNote: {} address(es) bound successfully.",
            successful_count
        )
        .ok();
    }

    msg.push_str("\nSuggestion: ");
    if failures
        .iter()
        .any(|f| f.error.kind() == std::io::ErrorKind::AddrInUse)
    {
        let ports: Vec<u16> = failures.iter().map(|f| f.address.port()).collect();
        let ports_str = if ports.len() == 1 {
            format!("port {}", ports[0])
        } else {
            format!(
                "ports {}",
                ports
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        write!(
            msg,
            "Check for processes using {} with 'lsof -i :<port>' or use different ports.",
            ports_str
        )
        .ok();
    } else if failures
        .iter()
        .any(|f| f.error.kind() == std::io::ErrorKind::PermissionDenied)
    {
        msg.push_str("Run with elevated privileges or use ports > 1024.");
    } else {
        msg.push_str("Check network configuration and address availability.");
    }

    msg
}

fn error_kind_to_hint(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::AddrInUse => "address already in use",
        std::io::ErrorKind::PermissionDenied => "permission denied",
        std::io::ErrorKind::AddrNotAvailable => "address not available",
        _ => "see error above",
    }
}
