//! Vendor-neutral capability matrix for a System Under Test.
//!
//! The conformance suite must run against any MQTT v5 broker. Different
//! brokers advertise different capabilities (spec-advertised values on
//! CONNACK), impose different resource limits, and behave differently within
//! the space the specification leaves open. This module models all three:
//!
//! * **Spec-advertised values** — `max_qos`, `retain_available`, etc.
//! * **Broker-imposed limits** — `max_clients`, `session_expiry_interval_max_secs`
//! * **Broker-specific behaviors** — `injected_user_properties`, `auth_failure_uses_disconnect`
//!
//! [`Requirement`] expresses a capability precondition for a single
//! conformance test; [`Requirement::matches`] checks it against a
//! [`Capabilities`] record. Tests that depend on optional features use
//! [`crate::skip::skip_if_missing`] to self-skip when the SUT doesn't meet
//! the preconditions.

use serde::Deserialize;

/// Full capability matrix for a System Under Test.
///
/// Deserialized from the `[capabilities]` table of a `sut.toml` descriptor.
/// All fields have defaults so partial TOML files are accepted; operators
/// only need to specify values that deviate from the permissive defaults.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub max_qos: u8,
    pub retain_available: bool,
    pub wildcard_subscription_available: bool,
    pub subscription_identifier_available: bool,
    pub shared_subscription_available: bool,
    pub topic_alias_maximum: u16,
    pub server_keep_alive: u16,
    pub server_receive_maximum: u16,
    pub maximum_packet_size: u32,
    pub assigned_client_id_supported: bool,

    pub max_clients: u32,
    pub max_subscriptions_per_client: u32,
    pub session_expiry_interval_max_secs: u32,
    pub enforces_inbound_receive_maximum: bool,

    pub injected_user_properties: Vec<String>,
    pub auth_failure_uses_disconnect: bool,
    pub unsupported_property_behavior: String,
    pub shared_subscription_distribution: String,

    pub strict_client_id_charset: bool,
    pub dollar_sys_publish: bool,

    pub transports: TransportSupport,
    pub enhanced_auth: EnhancedAuthSupport,
    pub acl: bool,
    pub hooks: HookSupport,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            max_qos: 2,
            retain_available: true,
            wildcard_subscription_available: true,
            subscription_identifier_available: true,
            shared_subscription_available: true,
            topic_alias_maximum: 65535,
            server_keep_alive: 0,
            server_receive_maximum: 65535,
            maximum_packet_size: 268_435_456,
            assigned_client_id_supported: true,
            max_clients: 0,
            max_subscriptions_per_client: 0,
            session_expiry_interval_max_secs: u32::MAX,
            enforces_inbound_receive_maximum: true,
            injected_user_properties: Vec::new(),
            auth_failure_uses_disconnect: false,
            unsupported_property_behavior: "DisconnectMalformed".to_owned(),
            shared_subscription_distribution: "RoundRobin".to_owned(),
            strict_client_id_charset: false,
            dollar_sys_publish: false,
            transports: TransportSupport::default(),
            enhanced_auth: EnhancedAuthSupport::default(),
            acl: false,
            hooks: HookSupport::default(),
        }
    }
}

/// Transport protocol support matrix.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TransportSupport {
    pub tcp: bool,
    pub tls: bool,
    pub websocket: bool,
    pub quic: bool,
}

impl Default for TransportSupport {
    fn default() -> Self {
        Self {
            tcp: true,
            tls: false,
            websocket: false,
            quic: false,
        }
    }
}

/// Enhanced authentication support declared by the SUT.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EnhancedAuthSupport {
    pub methods: Vec<String>,
}

/// Harness hook support declared by the SUT.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HookSupport {
    pub restart: bool,
    pub cleanup: bool,
}

/// A single capability precondition for a conformance test.
///
/// Tests enumerate their requirements; the test runner evaluates each against
/// the SUT's [`Capabilities`] and either runs the test or marks it skipped.
///
/// Variants intentionally form a typed enum rather than a string DSL so every
/// requirement is compile-time checked. All payloads are `Copy`/`'static` so
/// requirement arrays can be placed in `static` slots and collected via
/// `linkme::distributed_slice` for the CLI runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    TransportTcp,
    TransportTls,
    TransportWebSocket,
    TransportQuic,
    MinQos(u8),
    RetainAvailable,
    WildcardSubscriptionAvailable,
    SubscriptionIdentifierAvailable,
    SharedSubscriptionAvailable,
    EnhancedAuthMethod(&'static str),
    HookRestart,
    HookCleanup,
    Acl,
    StrictClientIdCharset,
    DollarSysPublish,
}

impl Requirement {
    /// Returns `true` if `capabilities` satisfies this requirement.
    #[must_use]
    pub fn matches(self, capabilities: &Capabilities) -> bool {
        match self {
            Self::TransportTcp => capabilities.transports.tcp,
            Self::TransportTls => capabilities.transports.tls,
            Self::TransportWebSocket => capabilities.transports.websocket,
            Self::TransportQuic => capabilities.transports.quic,
            Self::MinQos(q) => capabilities.max_qos >= q,
            Self::RetainAvailable => capabilities.retain_available,
            Self::WildcardSubscriptionAvailable => capabilities.wildcard_subscription_available,
            Self::SubscriptionIdentifierAvailable => capabilities.subscription_identifier_available,
            Self::SharedSubscriptionAvailable => capabilities.shared_subscription_available,
            Self::EnhancedAuthMethod(method) => capabilities
                .enhanced_auth
                .methods
                .iter()
                .any(|m| m == method),
            Self::HookRestart => capabilities.hooks.restart,
            Self::HookCleanup => capabilities.hooks.cleanup,
            Self::Acl => capabilities.acl,
            Self::StrictClientIdCharset => capabilities.strict_client_id_charset,
            Self::DollarSysPublish => capabilities.dollar_sys_publish,
        }
    }

    /// Returns a human-readable label for skip reasons.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::TransportTcp => "transport.tcp".to_owned(),
            Self::TransportTls => "transport.tls".to_owned(),
            Self::TransportWebSocket => "transport.websocket".to_owned(),
            Self::TransportQuic => "transport.quic".to_owned(),
            Self::MinQos(q) => format!("max_qos >= {q}"),
            Self::RetainAvailable => "retain_available".to_owned(),
            Self::WildcardSubscriptionAvailable => "wildcard_subscription_available".to_owned(),
            Self::SubscriptionIdentifierAvailable => "subscription_identifier_available".to_owned(),
            Self::SharedSubscriptionAvailable => "shared_subscription_available".to_owned(),
            Self::EnhancedAuthMethod(method) => format!("enhanced_auth.{method}"),
            Self::HookRestart => "hooks.restart".to_owned(),
            Self::HookCleanup => "hooks.cleanup".to_owned(),
            Self::Acl => "acl".to_owned(),
            Self::StrictClientIdCharset => "strict_client_id_charset".to_owned(),
            Self::DollarSysPublish => "dollar_sys_publish".to_owned(),
        }
    }

    /// Parses a string DSL form (as used in `#[conformance_test(requires = [...])]`)
    /// into a [`Requirement`].
    ///
    /// Accepted forms:
    /// * `transport.tcp`, `transport.tls`, `transport.websocket`, `transport.quic`
    /// * `max_qos>=N` where `N` is 0, 1, or 2
    /// * `retain_available`, `wildcard_subscription_available`,
    ///   `subscription_identifier_available`, `shared_subscription_available`
    /// * `enhanced_auth.<METHOD>` — `<METHOD>` is passed through as a
    ///   `&'static str` (callers must use string literals)
    /// * `hooks.restart`, `hooks.cleanup`
    /// * `acl`
    ///
    /// # Errors
    /// Returns the unrecognised spec string on failure. Intended for use at
    /// compile time inside the `#[conformance_test]` proc-macro to turn a
    /// string literal into the matching enum variant; runtime callers can
    /// also use this for dynamic configuration files.
    pub fn from_spec(spec: &'static str) -> Result<Self, &'static str> {
        if let Some(rest) = spec.strip_prefix("max_qos>=") {
            return match rest {
                "0" => Ok(Self::MinQos(0)),
                "1" => Ok(Self::MinQos(1)),
                "2" => Ok(Self::MinQos(2)),
                _ => Err(spec),
            };
        }
        if let Some(method) = spec.strip_prefix("enhanced_auth.") {
            return Ok(Self::EnhancedAuthMethod(method));
        }
        match spec {
            "transport.tcp" => Ok(Self::TransportTcp),
            "transport.tls" => Ok(Self::TransportTls),
            "transport.websocket" => Ok(Self::TransportWebSocket),
            "transport.quic" => Ok(Self::TransportQuic),
            "retain_available" => Ok(Self::RetainAvailable),
            "wildcard_subscription_available" => Ok(Self::WildcardSubscriptionAvailable),
            "subscription_identifier_available" => Ok(Self::SubscriptionIdentifierAvailable),
            "shared_subscription_available" => Ok(Self::SharedSubscriptionAvailable),
            "hooks.restart" => Ok(Self::HookRestart),
            "hooks.cleanup" => Ok(Self::HookCleanup),
            "acl" => Ok(Self::Acl),
            "strict_client_id_charset" => Ok(Self::StrictClientIdCharset),
            "dollar_sys_publish" => Ok(Self::DollarSysPublish),
            _ => Err(spec),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_capabilities_are_permissive() {
        let caps = Capabilities::default();
        assert_eq!(caps.max_qos, 2);
        assert!(caps.retain_available);
        assert!(caps.transports.tcp);
        assert!(!caps.transports.quic);
    }

    #[test]
    fn min_qos_requirement_matches_when_supported() {
        let qos2 = Capabilities {
            max_qos: 2,
            ..Capabilities::default()
        };
        assert!(Requirement::MinQos(1).matches(&qos2));
        assert!(Requirement::MinQos(2).matches(&qos2));
        let qos0 = Capabilities {
            max_qos: 0,
            ..Capabilities::default()
        };
        assert!(!Requirement::MinQos(1).matches(&qos0));
    }

    #[test]
    fn transport_requirement_matches_declared_transport() {
        let ws_on = Capabilities {
            transports: TransportSupport {
                websocket: true,
                ..TransportSupport::default()
            },
            ..Capabilities::default()
        };
        assert!(Requirement::TransportWebSocket.matches(&ws_on));
        let ws_off = Capabilities::default();
        assert!(!Requirement::TransportWebSocket.matches(&ws_off));
    }

    #[test]
    fn enhanced_auth_requirement_matches_declared_method() {
        let caps = Capabilities {
            enhanced_auth: EnhancedAuthSupport {
                methods: vec!["CHALLENGE-RESPONSE".to_owned()],
            },
            ..Capabilities::default()
        };
        assert!(Requirement::EnhancedAuthMethod("CHALLENGE-RESPONSE").matches(&caps));
        assert!(!Requirement::EnhancedAuthMethod("SCRAM-SHA-256").matches(&caps));
    }

    #[test]
    fn requirement_label_is_stable() {
        assert_eq!(Requirement::TransportTcp.label(), "transport.tcp");
        assert_eq!(Requirement::MinQos(2).label(), "max_qos >= 2");
        assert_eq!(
            Requirement::EnhancedAuthMethod("CR").label(),
            "enhanced_auth.CR"
        );
    }

    #[test]
    fn from_spec_parses_all_variants() {
        assert_eq!(
            Requirement::from_spec("transport.tcp").unwrap(),
            Requirement::TransportTcp
        );
        assert_eq!(
            Requirement::from_spec("max_qos>=2").unwrap(),
            Requirement::MinQos(2)
        );
        assert_eq!(
            Requirement::from_spec("enhanced_auth.CHALLENGE-RESPONSE").unwrap(),
            Requirement::EnhancedAuthMethod("CHALLENGE-RESPONSE")
        );
        assert_eq!(
            Requirement::from_spec("shared_subscription_available").unwrap(),
            Requirement::SharedSubscriptionAvailable
        );
        assert_eq!(Requirement::from_spec("acl").unwrap(), Requirement::Acl);
        assert!(Requirement::from_spec("unknown").is_err());
    }

    #[test]
    fn profiles_toml_parses_against_capabilities_schema() {
        use std::collections::BTreeMap;
        let text = include_str!("../profiles.toml");
        let profiles: BTreeMap<String, Capabilities> = toml::from_str(text)
            .expect("profiles.toml must deserialize as a map of name -> Capabilities");
        for required in ["core-broker", "core-broker-tls", "full-broker"] {
            assert!(
                profiles.contains_key(required),
                "profiles.toml is missing required profile `{required}`"
            );
        }
        let core = profiles.get("core-broker").unwrap();
        assert!(core.transports.tcp);
        assert!(!core.transports.tls);
        let core_tls = profiles.get("core-broker-tls").unwrap();
        assert!(core_tls.transports.tcp);
        assert!(core_tls.transports.tls);
        let full = profiles.get("full-broker").unwrap();
        assert!(full.transports.tcp);
        assert!(full.transports.tls);
        assert!(full.transports.websocket);
        assert!(full.acl);
    }

    #[test]
    fn capabilities_deserialize_from_partial_toml() {
        let toml_str = r"
            max_qos = 1
            retain_available = false

            [transports]
            tcp = true
            tls = true
        ";
        let caps: Capabilities = toml::from_str(toml_str).unwrap();
        assert_eq!(caps.max_qos, 1);
        assert!(!caps.retain_available);
        assert!(caps.transports.tcp);
        assert!(caps.transports.tls);
        assert!(!caps.transports.websocket);
        assert!(caps.wildcard_subscription_available);
    }
}
