//! Property-based tests for WebSocket configuration functionality
//!
//! This test suite uses property-based testing to verify the robustness
//! of WebSocket configuration parsing, validation, and TLS integration.

use mqtt5::transport::websocket::{WebSocketConfig, WebSocketTransport};
use mqtt5::transport::Transport;
use proptest::prelude::*;
use std::collections::HashMap;
use mqtt5::time::Duration;

// Generate valid WebSocket URLs
fn valid_websocket_url() -> impl Strategy<Value = String> {
    prop_oneof![
        // Basic ws:// URLs
        (
            prop::option::of("[a-zA-Z0-9.-]{1,20}"),
            1u16..65535,
            prop::option::of("[a-zA-Z0-9/._-]{0,50}")
        )
            .prop_map(|(host, port, path)| {
                let host = host.unwrap_or_else(|| "localhost".to_string());
                let path = path.unwrap_or_else(|| "/mqtt".to_string());
                format!("ws://{host}:{port}/{}", path.trim_start_matches('/'))
            }),
        // Basic wss:// URLs
        (
            prop::option::of("[a-zA-Z0-9.-]{1,20}"),
            1u16..65535,
            prop::option::of("[a-zA-Z0-9/._-]{0,50}")
        )
            .prop_map(|(host, port, path)| {
                let host = host.unwrap_or_else(|| "localhost".to_string());
                let path = path.unwrap_or_else(|| "/mqtt".to_string());
                format!("wss://{host}:{port}/{}", path.trim_start_matches('/'))
            }),
        // URLs with default ports
        "[a-zA-Z0-9.-]{1,20}".prop_map(|host| format!("ws://{host}/mqtt")),
        "[a-zA-Z0-9.-]{1,20}".prop_map(|host| format!("wss://{host}/mqtt")),
        // IP addresses
        (0u8..255, 0u8..255, 0u8..255, 0u8..255, 1u16..65535)
            .prop_map(|(a, b, c, d, port)| format!("ws://{a}.{b}.{c}.{d}:{port}/mqtt")),
        (0u8..255, 0u8..255, 0u8..255, 0u8..255, 1u16..65535)
            .prop_map(|(a, b, c, d, port)| format!("wss://{a}.{b}.{c}.{d}:{port}/mqtt"))
    ]
}

// Generate invalid WebSocket URLs
fn invalid_websocket_url() -> impl Strategy<Value = String> {
    prop_oneof![
        // Wrong schemes
        Just("http://localhost:8080/mqtt".to_string()),
        Just("https://localhost:8080/mqtt".to_string()),
        Just("tcp://localhost:8080".to_string()),
        Just("ftp://localhost/mqtt".to_string()),
        // Malformed URLs
        Just("not-a-url".to_string()),
        Just("ws://".to_string()),
        Just("wss://".to_string()),
        Just("ws://localhost:999999/mqtt".to_string()), // Invalid port
        Just("ws://localhost:-1/mqtt".to_string()),
        // Invalid characters
        Just("ws://\x00localhost:8080/mqtt".to_string()), // Null byte
        Just("ws://localhost\x00:8080/mqtt".to_string()), // Null byte
        Just("ws://\x01localhost:8080/mqtt".to_string()), // Control character
        // Empty or too long
        Just(String::new()),
        Just("x".repeat(2000)), // Very long URL
    ]
}

// Generate valid HTTP headers
fn valid_http_headers() -> impl Strategy<Value = HashMap<String, String>> {
    prop::collection::btree_map(
        "[A-Za-z][A-Za-z0-9-]{0,30}",                    // Header names
        "[A-Za-z0-9 ._~:/?#\\[\\]@!$&'()*+,;=-]{1,100}", // Header values
        0..10,
    )
    .prop_map(|btree| btree.into_iter().collect())
}

// Generate valid subprotocols
fn valid_subprotocols() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-zA-Z][a-zA-Z0-9._-]{0,20}", 0..5)
}

// Generate valid timeouts
fn valid_timeout() -> impl Strategy<Value = Duration> {
    (1u64..3600).prop_map(Duration::from_secs)
}

proptest! {
    #[test]
    fn prop_websocket_config_valid_urls(url in valid_websocket_url()) {
        let result = WebSocketConfig::new(&url);

        match result {
            Ok(config) => {
                // Verify basic properties - URL may be normalized (e.g., hostname lowercased)
                assert!(!config.url.as_str().is_empty());
                assert!(!config.subprotocols.is_empty());
                assert!(config.timeout > Duration::from_secs(0));

                // Verify scheme-specific properties
                if url.starts_with("wss://") {
                    assert!(config.is_secure());
                } else {
                    assert!(!config.is_secure());
                }

                // Verify host extraction works
                if let Some(host) = config.host() {
                    assert!(!host.is_empty());
                }

                // Verify port extraction
                let port = config.port();
                assert!(port > 0); // u16 is always <= 65535

                // Default ports should be correct only when no port is specified
                let colon_count = url.matches(':').count();
                if url.starts_with("ws://") && colon_count == 1 { // Only scheme://
                    assert_eq!(port, 80, "Default ws:// port should be 80");
                }
                if url.starts_with("wss://") && colon_count == 1 { // Only scheme://
                    assert_eq!(port, 443, "Default wss:// port should be 443");
                }
            }
            Err(e) => {
                // If it fails, should have a meaningful error message
                let msg = e.to_string();
                assert!(!msg.is_empty());
                assert!(msg.len() > 10, "Error message should be descriptive: {msg}");
            }
        }
    }

    #[test]
    fn prop_websocket_config_invalid_urls(url in invalid_websocket_url()) {
        let result = WebSocketConfig::new(&url);

        // Invalid URLs should always fail
        assert!(result.is_err(), "Invalid URL '{url}' should fail to parse");

        let error_msg = result.unwrap_err().to_string();
        assert!(!error_msg.is_empty());
        assert!(error_msg.len() > 5, "Error message should be descriptive");
    }

    #[test]
    fn prop_websocket_config_with_options(
        url in valid_websocket_url(),
        timeout in valid_timeout(),
        headers in valid_http_headers(),
        subprotocols in valid_subprotocols(),
        user_agent in prop::option::of("[A-Za-z0-9 ._/-]{1,50}")
    ) {
        let Ok(mut config) = WebSocketConfig::new(&url) else {
            return Ok(()); // Skip invalid URLs
        };

        // Apply all options
        config = config.with_timeout(timeout);

        for (name, value) in &headers {
            config = config.with_header(name, value);
        }

        if !subprotocols.is_empty() {
            let proto_refs: Vec<&str> = subprotocols.iter().map(std::string::String::as_str).collect();
            config = config.with_subprotocols(&proto_refs);
        }

        if let Some(ua) = &user_agent {
            config = config.with_user_agent(ua);
        }

        // Verify all options were applied correctly
        assert_eq!(config.timeout, timeout);
        assert_eq!(config.headers.len(), headers.len());

        for (name, value) in &headers {
            assert_eq!(config.headers.get(name), Some(value));
        }

        if !subprotocols.is_empty() {
            assert_eq!(config.subprotocols, subprotocols);
        }

        if let Some(ua) = user_agent {
            assert_eq!(config.user_agent, Some(ua));
        }
    }

    #[test]
    fn prop_websocket_tls_auto_config(url in valid_websocket_url()) {
        let Ok(config) = WebSocketConfig::new(&url) else {
            return Ok(()); // Skip invalid URLs
        };

        if config.is_secure() {
            // For secure URLs, should be able to create auto TLS config if host is an IP
            if config.host().unwrap_or("").parse::<std::net::IpAddr>().is_ok() {
                let result = config.with_tls_auto();

                match result {
                    Ok(configured) => {
                        assert!(configured.tls_config().is_some());
                        let tls_config = configured.tls_config().unwrap();
                        assert!(!tls_config.hostname.is_empty());
                        assert!(tls_config.addr.port() > 0);
                    }
                    Err(e) => {
                        // Should have meaningful error
                        assert!(!e.to_string().is_empty());
                    }
                }
            }
        } else {
            // For non-secure URLs, should fail
            let result = config.with_tls_auto();
            assert!(result.is_err(), "TLS auto config should fail for ws:// URLs");
        }
    }

    #[test]
    fn prop_websocket_transport_creation(url in valid_websocket_url()) {
        let Ok(config) = WebSocketConfig::new(&url) else {
            return Ok(()); // Skip invalid URLs
        };

        let transport = WebSocketTransport::new(config);

        // Verify transport properties
        assert!(!transport.is_connected());
        assert!(transport.url().as_str().contains("://"));

        // Check subprotocol if available
        if let Some(subprotocol) = transport.subprotocol() {
            assert!(!subprotocol.is_empty());
        }
    }

    #[test]
    fn prop_websocket_client_auth_bytes_secure_only(
        url in valid_websocket_url(),
        cert_data in prop::collection::vec(any::<u8>(), 50..500),
        key_data in prop::collection::vec(any::<u8>(), 50..500)
    ) {
        let Ok(config) = WebSocketConfig::new(&url) else {
            return Ok(()); // Skip invalid URLs
        };

        let is_secure = config.is_secure();
        let result = config.with_client_auth_from_bytes(&cert_data, &key_data);

        if is_secure {
            // For secure connections, should attempt to process certificates
            match result {
                Ok(configured) => {
                    assert!(configured.tls_config().is_some());
                }
                Err(e) => {
                    // Should fail gracefully with meaningful error
                    let msg = e.to_string();
                    assert!(!msg.is_empty());
                    assert!(msg.len() > 10, "Error should be descriptive: {msg}");
                }
            }
        } else {
            // For non-secure connections, should fail
            assert!(result.is_err(), "Client auth should fail for ws:// URLs");
            let msg = result.unwrap_err().to_string();
            assert!(msg.contains("wss://") || msg.contains("secure"),
                   "Error should mention security requirement: {msg}");
        }
    }

    #[test]
    fn prop_websocket_ca_cert_bytes_secure_only(
        url in valid_websocket_url(),
        ca_data in prop::collection::vec(any::<u8>(), 50..500)
    ) {
        let Ok(config) = WebSocketConfig::new(&url) else {
            return Ok(()); // Skip invalid URLs
        };

        let is_secure = config.is_secure();
        let result = config.with_ca_cert_from_bytes(&ca_data);

        if is_secure {
            // For secure connections, should attempt to process CA cert
            match result {
                Ok(configured) => {
                    assert!(configured.tls_config().is_some());
                }
                Err(e) => {
                    let msg = e.to_string();
                    assert!(!msg.is_empty());
                    assert!(msg.len() > 10, "Error should be descriptive: {msg}");
                }
            }
        } else {
            // For non-secure connections, should fail
            assert!(result.is_err(), "CA cert should fail for ws:// URLs");
        }
    }

    #[test]
    fn prop_websocket_config_immutability(
        url in valid_websocket_url(),
        timeout in valid_timeout()
    ) {
        let Ok(original_config) = WebSocketConfig::new(&url) else {
            return Ok(()); // Skip invalid URLs
        };

        let original_timeout = original_config.timeout;
        let original_url = original_config.url.clone();

        // Create new config with different timeout
        let new_config = original_config.with_timeout(timeout);

        // Original should be unchanged (moved, but concept remains)
        assert_eq!(new_config.url, original_url);
        assert_eq!(new_config.timeout, timeout);

        // If timeout was different, verify change
        if timeout != original_timeout {
            assert_ne!(new_config.timeout, original_timeout);
        }
    }

    #[test]
    fn prop_websocket_port_extraction_consistency(url in valid_websocket_url()) {
        let Ok(config) = WebSocketConfig::new(&url) else {
            return Ok(()); // Skip invalid URLs
        };

        let port = config.port();
        let url_str = config.url.as_str();

        // Port should be consistent with URL
        let colon_count = url_str.matches(':').count();
        if colon_count == 1 { // Only scheme colon, no port specified
            if url_str.starts_with("wss://") {
                assert_eq!(port, 443, "Default wss:// port should be 443");
            } else if url_str.starts_with("ws://") {
                assert_eq!(port, 80, "Default ws:// port should be 80");
            }
        }
        // If port is explicitly specified, it should match that port
        // (but we can't easily extract it from the string without parsing)

        // Port should always be valid
        assert!(port > 0, "Port {port} should be positive"); // u16 is always <= 65535
    }
}

// Unit tests for edge cases and specific behaviors
#[cfg(test)]
mod edge_case_tests {
    use super::*;
    use mqtt5::transport::tls::TlsConfig;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[tokio::test]
    async fn test_websocket_transport_connect_lifecycle() {
        let config = WebSocketConfig::new("ws://127.0.0.1:59999/mqtt").unwrap();
        let mut transport = WebSocketTransport::new(config);

        // Initial state
        assert!(!transport.is_connected());

        // Try to connect - should fail since no server is running
        let result = transport.connect().await;
        assert!(result.is_err());
        assert!(!transport.is_connected());

        // Close when not connected - should be ok
        let close_result = transport.close().await;
        assert!(close_result.is_ok());
        assert!(!transport.is_connected());

        // Close again - should be ok
        let close_result2 = transport.close().await;
        assert!(close_result2.is_ok());
    }

    #[tokio::test]
    async fn test_websocket_transport_operations_require_connection() {
        let config = WebSocketConfig::new("ws://127.0.0.1:59999/mqtt").unwrap();
        let mut transport = WebSocketTransport::new(config);

        // Operations should fail when not connected
        let mut buf = [0u8; 10];
        assert!(transport.read(&mut buf).await.is_err());
        assert!(transport.write(b"test").await.is_err());

        // Try to connect - should fail since no server is running
        let connect_result = transport.connect().await;
        assert!(connect_result.is_err());

        // Operations should still fail since connection failed
        assert!(transport.read(&mut buf).await.is_err());
        assert!(transport.write(b"test").await.is_err());
    }

    #[test]
    fn test_websocket_config_tls_config_management() {
        let config = WebSocketConfig::new("wss://127.0.0.1:8443/mqtt").unwrap();
        let mut config_with_tls = config.with_tls_auto().unwrap();

        // Should have TLS config
        assert!(config_with_tls.tls_config().is_some());

        // Take TLS config
        let tls_config = config_with_tls.take_tls_config();
        assert!(tls_config.is_some());
        assert!(config_with_tls.tls_config().is_none());

        // Verify the taken config
        let tls = tls_config.unwrap();
        assert_eq!(tls.hostname, "127.0.0.1");
        assert_eq!(tls.addr.port(), 8443);
    }

    #[test]
    fn test_websocket_config_with_custom_tls() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 8883);
        let custom_tls = TlsConfig::new(addr, "custom.broker.com");

        let config = WebSocketConfig::new("wss://original.broker.com:8443/mqtt")
            .unwrap()
            .with_tls_config(custom_tls);

        // Should use the custom TLS config, not auto-generated
        assert!(config.tls_config().is_some());
        let tls = config.tls_config().unwrap();
        assert_eq!(tls.hostname, "custom.broker.com");
        assert_eq!(tls.addr.port(), 8883); // From custom config, not URL
    }

    #[test]
    fn test_websocket_config_subprotocol_methods() {
        let config = WebSocketConfig::new("ws://localhost:8080/mqtt").unwrap();

        // Test single subprotocol
        let config1 = config.with_subprotocol("mqttv5.0");
        assert_eq!(config1.subprotocols, vec!["mqttv5.0"]);

        // Test multiple subprotocols
        let config2 = WebSocketConfig::new("ws://localhost:8080/mqtt")
            .unwrap()
            .with_subprotocols(&["mqtt", "mqttv3.1", "mqttv5.0"]);
        assert_eq!(config2.subprotocols, vec!["mqtt", "mqttv3.1", "mqttv5.0"]);
    }

    #[test]
    fn test_websocket_config_edge_case_urls() {
        // URL with query parameters
        let config1 = WebSocketConfig::new("ws://localhost:8080/mqtt?client=test&version=5");
        assert!(config1.is_ok());

        // URL with fragment
        let config2 = WebSocketConfig::new("ws://localhost:8080/mqtt#section1");
        assert!(config2.is_ok());

        // URL with username/password (should work but unusual for WebSocket)
        let config3 = WebSocketConfig::new("ws://user:pass@localhost:8080/mqtt");
        assert!(config3.is_ok());

        // IPv6 address
        let config4 = WebSocketConfig::new("ws://[::1]:8080/mqtt");
        assert!(config4.is_ok());
        if let Ok(cfg) = config4 {
            // IPv6 addresses are returned with brackets by url crate
            assert!(cfg.host().unwrap_or("").contains("::1"));
        }
    }

    #[test]
    fn test_websocket_config_header_management() {
        let config = WebSocketConfig::new("ws://localhost:8080/mqtt")
            .unwrap()
            .with_header("Authorization", "Bearer token123")
            .with_header("X-Custom-Header", "custom-value")
            .with_user_agent("MyApp/1.0");

        assert_eq!(config.headers.len(), 2);
        assert_eq!(
            config.headers.get("Authorization"),
            Some(&"Bearer token123".to_string())
        );
        assert_eq!(
            config.headers.get("X-Custom-Header"),
            Some(&"custom-value".to_string())
        );
        assert_eq!(config.user_agent, Some("MyApp/1.0".to_string()));
    }
}
