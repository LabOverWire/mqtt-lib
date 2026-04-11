//! System Under Test abstraction.
//!
//! A System Under Test (SUT) is any MQTT v5 broker the conformance suite is
//! configured to validate. Operators describe their broker with a
//! [`SutDescriptor`] loaded from a `sut.toml` file; tests interact with it
//! through a [`SutHandle`] that is agnostic to whether the broker is running
//! in-process (for mqtt-lib's self-tests) or as an external process (for
//! third-party brokers).
//!
//! In-process operation is gated behind the `inprocess-fixture` feature.
//! When the feature is absent, the crate depends on nothing from `mqtt5` and
//! can be extracted into a standalone repository.

use crate::capabilities::Capabilities;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;

/// Errors produced while loading or resolving a [`SutDescriptor`].
#[derive(Debug)]
pub enum SutDescriptorError {
    /// Failed to read the descriptor file from disk.
    Io(std::io::Error),
    /// Failed to parse the descriptor as TOML.
    Parse(toml::de::Error),
    /// Failed to parse a listed address as a `SocketAddr`.
    InvalidAddress {
        field: &'static str,
        value: String,
        source: std::net::AddrParseError,
    },
}

impl std::fmt::Display for SutDescriptorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "failed to read SUT descriptor: {e}"),
            Self::Parse(e) => write!(f, "failed to parse SUT descriptor: {e}"),
            Self::InvalidAddress {
                field,
                value,
                source,
            } => {
                write!(f, "invalid address in `{field}` = {value:?}: {source}")
            }
        }
    }
}

impl std::error::Error for SutDescriptorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Parse(e) => Some(e),
            Self::InvalidAddress { source, .. } => Some(source),
        }
    }
}

/// Harness-invocable hooks exposed by the operator.
///
/// All fields are optional. An absent command means the hook is unsupported
/// and tests that require it will self-skip.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SutHooks {
    pub restart_command: String,
    pub cleanup_command: String,
}

/// A fully-parsed `sut.toml` descriptor.
///
/// Describes how to reach the broker (addresses), what it claims to support
/// (capabilities), and how to manage its lifecycle (hooks). Produced from a
/// TOML file by [`SutDescriptor::from_file`] or [`SutDescriptor::from_str`].
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SutDescriptor {
    pub name: String,
    pub address: String,
    pub tls_address: String,
    pub ws_address: String,
    pub capabilities: Capabilities,
    pub hooks: SutHooks,
}

impl Default for SutDescriptor {
    fn default() -> Self {
        Self {
            name: "unnamed-sut".to_owned(),
            address: String::new(),
            tls_address: String::new(),
            ws_address: String::new(),
            capabilities: Capabilities::default(),
            hooks: SutHooks::default(),
        }
    }
}

impl SutDescriptor {
    /// Loads and parses a descriptor from a TOML file on disk.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or the TOML is invalid.
    pub fn from_file(path: &Path) -> Result<Self, SutDescriptorError> {
        let text = std::fs::read_to_string(path).map_err(SutDescriptorError::Io)?;
        Self::from_str(&text)
    }

    /// Parses a descriptor from a TOML string.
    ///
    /// # Errors
    /// Returns an error if the TOML is invalid.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Result<Self, SutDescriptorError> {
        let mut descriptor: Self = toml::from_str(text).map_err(SutDescriptorError::Parse)?;
        if !descriptor.hooks.restart_command.is_empty() {
            descriptor.capabilities.hooks.restart = true;
        }
        if !descriptor.hooks.cleanup_command.is_empty() {
            descriptor.capabilities.hooks.cleanup = true;
        }
        Ok(descriptor)
    }

    /// Returns the plain-TCP `SocketAddr` if the descriptor declares one.
    ///
    /// Accepts both `mqtt://host:port` and `host:port` forms; the scheme is
    /// stripped before parsing.
    ///
    /// # Errors
    /// Returns an error if the address field is set but not a valid
    /// `SocketAddr`.
    pub fn tcp_socket_addr(&self) -> Result<Option<SocketAddr>, SutDescriptorError> {
        parse_optional_addr("address", &self.address)
    }

    /// Returns the TLS `SocketAddr` if the descriptor declares one.
    ///
    /// # Errors
    /// Returns an error if the field is set but not a valid `SocketAddr`.
    pub fn tls_socket_addr(&self) -> Result<Option<SocketAddr>, SutDescriptorError> {
        parse_optional_addr("tls_address", &self.tls_address)
    }

    /// Returns the WebSocket `SocketAddr` if the descriptor declares one.
    ///
    /// The WebSocket path is implied by the conformance test (`/mqtt`); only
    /// the host:port portion is extracted here.
    ///
    /// # Errors
    /// Returns an error if the field is set but not a valid `SocketAddr`.
    pub fn ws_socket_addr(&self) -> Result<Option<SocketAddr>, SutDescriptorError> {
        parse_optional_addr("ws_address", &self.ws_address)
    }
}

fn parse_optional_addr(
    field: &'static str,
    value: &str,
) -> Result<Option<SocketAddr>, SutDescriptorError> {
    if value.is_empty() {
        return Ok(None);
    }
    let stripped = strip_scheme(value);
    let stripped = strip_ws_path(stripped);
    stripped
        .parse()
        .map(Some)
        .map_err(|source| SutDescriptorError::InvalidAddress {
            field,
            value: value.to_owned(),
            source,
        })
}

fn strip_scheme(addr: &str) -> &str {
    for scheme in ["mqtts://", "mqtt://", "wss://", "ws://"] {
        if let Some(rest) = addr.strip_prefix(scheme) {
            return rest;
        }
    }
    addr
}

fn strip_ws_path(addr: &str) -> &str {
    addr.split('/').next().unwrap_or(addr)
}

/// A live handle to a running System Under Test.
///
/// Exposes only what tests need: the capability matrix and per-transport
/// addresses. Tests never access `mqtt5::*` types through this handle, which
/// is what allows the crate to be extracted from its host repository.
///
/// Both variants carry boxed payloads so the enum stays small regardless of
/// whether the `inprocess-fixture` feature is enabled.
pub enum SutHandle {
    External(Box<SutDescriptor>),
    #[cfg(feature = "inprocess-fixture")]
    InProcess(Box<InProcessFixture>),
}

impl SutHandle {
    /// Returns the capability matrix of the underlying SUT.
    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        match self {
            Self::External(descriptor) => &descriptor.capabilities,
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(fixture) => fixture.capabilities(),
        }
    }

    /// Returns the SUT's primary TCP `SocketAddr`, panicking with a clear
    /// message if the SUT does not declare one or if the declared address is
    /// malformed. Intended for test bodies that require a working TCP
    /// endpoint; production code should use [`Self::tcp_socket_addr`].
    ///
    /// # Panics
    /// Panics if the SUT has no TCP address or if the declared address
    /// cannot be parsed as a `SocketAddr`.
    #[must_use]
    pub fn expect_tcp_addr(&self) -> SocketAddr {
        self.tcp_socket_addr()
            .expect("sut tcp address must parse")
            .expect("sut must declare a tcp address")
    }

    /// Returns the SUT's primary TCP address, if any.
    ///
    /// # Errors
    /// Returns an error if the external descriptor's TCP address is
    /// malformed.
    pub fn tcp_socket_addr(&self) -> Result<Option<SocketAddr>, SutDescriptorError> {
        match self {
            Self::External(descriptor) => descriptor.tcp_socket_addr(),
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(fixture) => Ok(Some(fixture.tcp_socket_addr())),
        }
    }

    /// Returns the SUT's WebSocket address, if any.
    ///
    /// # Errors
    /// Returns an error if the external descriptor's WebSocket address is
    /// malformed.
    pub fn ws_socket_addr(&self) -> Result<Option<SocketAddr>, SutDescriptorError> {
        match self {
            Self::External(descriptor) => descriptor.ws_socket_addr(),
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(fixture) => Ok(fixture.ws_socket_addr()),
        }
    }

    /// Returns the SUT's WebSocket `SocketAddr`, panicking with a clear
    /// message if the SUT does not declare one or if the declared address is
    /// malformed.
    ///
    /// # Panics
    /// Panics if the SUT has no WebSocket address or if the declared address
    /// cannot be parsed as a `SocketAddr`.
    #[must_use]
    pub fn expect_ws_addr(&self) -> SocketAddr {
        self.ws_socket_addr()
            .expect("sut ws address must parse")
            .expect("sut must declare a ws address")
    }
}

/// An in-process SUT fixture that owns a running broker.
///
/// Wraps the existing [`crate::harness::ConformanceBroker`] so the two code
/// paths (in-process and external) converge behind the same [`SutHandle`]
/// enum. Only available when the `inprocess-fixture` feature is enabled.
#[cfg(feature = "inprocess-fixture")]
pub struct InProcessFixture {
    broker: crate::harness::ConformanceBroker,
    capabilities: Capabilities,
}

#[cfg(feature = "inprocess-fixture")]
impl InProcessFixture {
    /// Starts the default in-process broker and returns a fixture wrapping it.
    pub async fn start() -> Self {
        let broker = crate::harness::ConformanceBroker::start().await;
        Self {
            broker,
            capabilities: inprocess_capabilities(false),
        }
    }

    /// Starts the in-process broker with WebSocket enabled.
    pub async fn start_with_websocket() -> Self {
        let broker = crate::harness::ConformanceBroker::start_with_websocket().await;
        Self {
            broker,
            capabilities: inprocess_capabilities(true),
        }
    }

    /// Starts the in-process broker with a caller-supplied configuration.
    ///
    /// Intended for tests that need to exercise non-default broker settings
    /// (for example, a reduced `server_receive_maximum`). Only available with
    /// the `inprocess-fixture` feature; external SUTs express the same
    /// constraints through `sut.toml` capabilities.
    pub async fn start_with_config(config: mqtt5::broker::config::BrokerConfig) -> Self {
        let broker = crate::harness::ConformanceBroker::start_with_config(config).await;
        Self {
            broker,
            capabilities: inprocess_capabilities(false),
        }
    }

    /// Starts the in-process broker with a custom authentication provider.
    ///
    /// Used by enhanced-auth conformance tests that need to inject a
    /// challenge-response provider. Only available with the
    /// `inprocess-fixture` feature; external SUTs declare enhanced-auth
    /// support through `sut.toml` capabilities.
    pub async fn start_with_auth_provider(
        provider: std::sync::Arc<dyn mqtt5::broker::auth::AuthProvider>,
    ) -> Self {
        let broker = crate::harness::ConformanceBroker::start_with_auth_provider(provider).await;
        Self {
            broker,
            capabilities: inprocess_capabilities(false),
        }
    }

    /// Returns the underlying [`crate::harness::ConformanceBroker`].
    ///
    /// Phase F will strip direct uses of this accessor as tests migrate to
    /// the [`SutHandle`]-driven API.
    #[must_use]
    pub fn broker(&self) -> &crate::harness::ConformanceBroker {
        &self.broker
    }

    /// Returns the in-process TCP socket address.
    #[must_use]
    pub fn tcp_socket_addr(&self) -> SocketAddr {
        self.broker.socket_addr()
    }

    /// Returns the in-process WebSocket socket address, if enabled.
    #[must_use]
    pub fn ws_socket_addr(&self) -> Option<SocketAddr> {
        self.broker
            .ws_port()
            .map(|port| SocketAddr::from(([127, 0, 0, 1], port)))
    }

    /// Returns this fixture's capability matrix.
    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
}

/// Starts the default in-process broker and returns it wrapped in a
/// [`SutHandle::InProcess`] ready for use in conformance tests.
///
/// This is the canonical one-liner used by every in-process integration
/// test: it eliminates per-file boilerplate that otherwise re-defines the
/// same `Box::new(InProcessFixture::start().await)` dance.
#[cfg(feature = "inprocess-fixture")]
pub async fn inprocess_sut() -> SutHandle {
    SutHandle::InProcess(Box::new(InProcessFixture::start().await))
}

/// Starts an in-process broker with WebSocket enabled and returns it
/// wrapped in a [`SutHandle::InProcess`].
#[cfg(feature = "inprocess-fixture")]
pub async fn inprocess_sut_with_websocket() -> SutHandle {
    SutHandle::InProcess(Box::new(InProcessFixture::start_with_websocket().await))
}

/// Starts an in-process broker with a caller-supplied configuration and
/// returns it wrapped in a [`SutHandle::InProcess`].
///
/// Used by conformance tests that need to override default broker settings
/// (for example, to validate behaviour at a reduced receive-maximum).
#[cfg(feature = "inprocess-fixture")]
pub async fn inprocess_sut_with_config(config: mqtt5::broker::config::BrokerConfig) -> SutHandle {
    SutHandle::InProcess(Box::new(InProcessFixture::start_with_config(config).await))
}

/// Starts an in-process broker with a caller-supplied auth provider and
/// returns it wrapped in a [`SutHandle::InProcess`].
///
/// Used by enhanced-auth conformance tests to inject a challenge-response
/// provider. Only available when the `inprocess-fixture` feature is enabled.
#[cfg(feature = "inprocess-fixture")]
pub async fn inprocess_sut_with_auth_provider(
    provider: std::sync::Arc<dyn mqtt5::broker::auth::AuthProvider>,
) -> SutHandle {
    SutHandle::InProcess(Box::new(
        InProcessFixture::start_with_auth_provider(provider).await,
    ))
}

#[cfg(feature = "inprocess-fixture")]
fn inprocess_capabilities(websocket: bool) -> Capabilities {
    Capabilities {
        transports: crate::capabilities::TransportSupport {
            tcp: true,
            tls: false,
            websocket,
            quic: false,
        },
        injected_user_properties: vec!["x-mqtt-sender".to_owned(), "x-mqtt-client-id".to_owned()],
        ..Capabilities::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_descriptor() {
        let toml_str = r#"
            name = "mosquitto-2.x"
            address = "mqtt://127.0.0.1:1883"
        "#;
        let sut = SutDescriptor::from_str(toml_str).unwrap();
        assert_eq!(sut.name, "mosquitto-2.x");
        assert_eq!(sut.address, "mqtt://127.0.0.1:1883");
        assert_eq!(
            sut.tcp_socket_addr().unwrap().unwrap().to_string(),
            "127.0.0.1:1883"
        );
    }

    #[test]
    fn parse_descriptor_with_capabilities() {
        let toml_str = r#"
            name = "restricted"
            address = "127.0.0.1:1883"

            [capabilities]
            max_qos = 1
            retain_available = false
            injected_user_properties = ["x-mqtt-sender"]
        "#;
        let sut = SutDescriptor::from_str(toml_str).unwrap();
        assert_eq!(sut.capabilities.max_qos, 1);
        assert!(!sut.capabilities.retain_available);
        assert_eq!(
            sut.capabilities.injected_user_properties,
            vec!["x-mqtt-sender".to_owned()]
        );
    }

    #[test]
    fn absent_addresses_return_none() {
        let sut = SutDescriptor::default();
        assert_eq!(sut.tcp_socket_addr().unwrap(), None);
        assert_eq!(sut.tls_socket_addr().unwrap(), None);
        assert_eq!(sut.ws_socket_addr().unwrap(), None);
    }

    #[test]
    fn malformed_address_reports_field() {
        let sut = SutDescriptor {
            address: "not-an-address".to_owned(),
            ..SutDescriptor::default()
        };
        let err = sut.tcp_socket_addr().unwrap_err();
        match err {
            SutDescriptorError::InvalidAddress { field, .. } => assert_eq!(field, "address"),
            other => panic!("expected InvalidAddress, got {other:?}"),
        }
    }

    #[test]
    fn strip_ws_path_removes_suffix() {
        assert_eq!(strip_ws_path("127.0.0.1:8083/mqtt"), "127.0.0.1:8083");
        assert_eq!(strip_ws_path("127.0.0.1:8083"), "127.0.0.1:8083");
    }

    #[test]
    fn fixture_inprocess_parses() {
        let text = include_str!("../tests/fixtures/inprocess.toml");
        let descriptor = SutDescriptor::from_str(text).expect("inprocess fixture must parse");
        assert_eq!(descriptor.name, "mqtt5-inprocess");
        assert!(descriptor.capabilities.transports.tcp);
        assert!(!descriptor.capabilities.transports.tls);
    }

    #[test]
    fn fixture_external_mqtt5_parses() {
        let text = include_str!("../tests/fixtures/external-mqtt5.toml");
        let descriptor = SutDescriptor::from_str(text).expect("external-mqtt5 fixture must parse");
        assert_eq!(descriptor.name, "mqtt5-external");
        assert!(descriptor.capabilities.transports.tcp);
        assert!(!descriptor.capabilities.transports.tls);
        assert_eq!(
            descriptor.tcp_socket_addr().unwrap().unwrap().to_string(),
            "127.0.0.1:1883"
        );
    }

    #[test]
    fn fixture_mosquitto_parses() {
        let text = include_str!("../tests/fixtures/mosquitto-2.x.toml");
        let descriptor = SutDescriptor::from_str(text).expect("mosquitto fixture must parse");
        assert_eq!(descriptor.name, "mosquitto-2.x");
        assert!(descriptor.capabilities.shared_subscription_available);
    }

    #[test]
    fn fixture_emqx_parses() {
        let text = include_str!("../tests/fixtures/emqx.toml");
        let descriptor = SutDescriptor::from_str(text).expect("emqx fixture must parse");
        assert_eq!(descriptor.name, "emqx-5.x");
        assert!(descriptor.capabilities.transports.quic);
        assert!(descriptor.capabilities.acl);
        assert!(descriptor
            .capabilities
            .enhanced_auth
            .methods
            .iter()
            .any(|m| m == "SCRAM-SHA-256"));
    }

    #[test]
    fn fixture_hivemq_ce_parses() {
        let text = include_str!("../tests/fixtures/hivemq-ce.toml");
        let descriptor = SutDescriptor::from_str(text).expect("hivemq-ce fixture must parse");
        assert_eq!(descriptor.name, "hivemq-ce");
        assert!(descriptor.capabilities.transports.websocket);
        assert!(!descriptor.capabilities.acl);
    }

    #[test]
    fn ws_address_parses_with_path() {
        let sut = SutDescriptor {
            ws_address: "ws://127.0.0.1:8083/mqtt".to_owned(),
            ..SutDescriptor::default()
        };
        assert_eq!(
            sut.ws_socket_addr().unwrap().unwrap().to_string(),
            "127.0.0.1:8083"
        );
    }
}
