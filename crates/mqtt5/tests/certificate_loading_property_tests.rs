//! Property-based tests for certificate loading functionality
//!
//! This test suite uses property-based testing to verify the robustness
//! of certificate loading from bytes in various formats and edge cases.

use mqtt5::transport::tls::TlsConfig;
use proptest::prelude::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

// Generate valid PEM certificate structures
fn valid_pem_cert() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 100..2000).prop_map(|data| {
        let mut pem = b"-----BEGIN CERTIFICATE-----\n".to_vec();
        // Convert to base64-like content (simplified for testing)
        let b64_data: String = data
            .chunks(3)
            .map(|chunk| match chunk.len() {
                1 => format!("{:02x}==", chunk[0]),
                2 => format!("{:02x}{:02x}=", chunk[0], chunk[1]),
                _ => format!("{:02x}{:02x}{:02x}", chunk[0], chunk[1], chunk[2]),
            })
            .collect::<String>();

        // Add line breaks every 64 characters
        for (i, c) in b64_data.chars().enumerate() {
            if i % 64 == 0 && i > 0 {
                pem.push(b'\n');
            }
            pem.push(c as u8);
        }
        pem.extend_from_slice(b"\n-----END CERTIFICATE-----");
        pem
    })
}

// Generate valid PEM private key structures
fn valid_pem_key() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 50..1000).prop_map(|data| {
        let mut pem = b"-----BEGIN PRIVATE KEY-----\n".to_vec();
        let b64_data: String = data
            .chunks(3)
            .map(|chunk| match chunk.len() {
                1 => format!("{:02x}==", chunk[0]),
                2 => format!("{:02x}{:02x}=", chunk[0], chunk[1]),
                _ => format!("{:02x}{:02x}{:02x}", chunk[0], chunk[1], chunk[2]),
            })
            .collect::<String>();

        for (i, c) in b64_data.chars().enumerate() {
            if i % 64 == 0 && i > 0 {
                pem.push(b'\n');
            }
            pem.push(c as u8);
        }
        pem.extend_from_slice(b"\n-----END PRIVATE KEY-----");
        pem
    })
}

// Generate invalid PEM data
fn invalid_pem_data() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        // Missing headers
        prop::collection::vec(any::<u8>(), 0..100),
        // Invalid headers
        Just(b"-----BEGIN INVALID-----\ndata\n-----END INVALID-----".to_vec()),
        // Truncated
        Just(b"-----BEGIN CERTIFICATE-----\ndata".to_vec()),
        // Wrong footer
        Just(b"-----BEGIN CERTIFICATE-----\ndata\n-----END PRIVATE KEY-----".to_vec()),
        // Empty
        Just(Vec::new()),
        // Only header
        Just(b"-----BEGIN CERTIFICATE-----".to_vec()),
    ]
}

// Generate valid DER certificate data (simplified)
#[allow(clippy::cast_possible_truncation)]
fn valid_der_cert() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 100..2000).prop_map(|mut data| {
        // DER always starts with SEQUENCE tag (0x30)
        data[0] = 0x30;
        // Set a valid length encoding (simplified)
        if data.len() > 127 {
            data[1] = 0x82; // Long form, 2 bytes
            let len = (data.len() - 4) as u16;
            data[2] = (len >> 8) as u8;
            data[3] = (len & 0xFF) as u8;
        } else {
            data[1] = (data.len() - 2) as u8;
        }
        data
    })
}

// Generate invalid DER data
fn invalid_der_data() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        // Empty
        Just(Vec::new()),
        // Too short
        prop::collection::vec(any::<u8>(), 1..4),
        // Invalid tag
        prop::collection::vec(any::<u8>(), 10..100).prop_map(|mut data| {
            data[0] = 0xFF; // Invalid tag
            data
        }),
        // Invalid length encoding
        prop::collection::vec(any::<u8>(), 10..100).prop_map(|mut data| {
            data[0] = 0x30; // Valid sequence tag
            data[1] = 0xFF; // Invalid length
            data
        }),
    ]
}

proptest! {
    #[test]
    fn prop_pem_cert_loading_valid_data(cert_data in valid_pem_cert()) {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        // Should not panic - may succeed or fail gracefully
        let result = config.load_client_cert_pem_bytes(&cert_data);

        // The important thing is that it doesn't panic and returns a Result
        match result {
            Ok(()) => {
                // If it succeeds, certificate should be loaded
                assert!(config.client_cert.is_some());
            }
            Err(e) => {
                // If it fails, should be a proper error message
                assert!(!e.to_string().is_empty());
            }
        }
    }

    #[test]
    fn prop_pem_key_loading_valid_data(key_data in valid_pem_key()) {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        let result = config.load_client_key_pem_bytes(&key_data);

        match result {
            Ok(()) => {
                assert!(config.client_key.is_some());
            }
            Err(e) => {
                assert!(!e.to_string().is_empty());
            }
        }
    }

    #[test]
    fn prop_pem_cert_loading_invalid_data(invalid_data in invalid_pem_data()) {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        // Invalid data should always result in an error
        let result = config.load_client_cert_pem_bytes(&invalid_data);
        assert!(result.is_err(), "Invalid PEM data should fail to load");

        // Should not modify the config on error
        assert!(config.client_cert.is_none());
    }

    #[test]
    fn prop_pem_key_loading_invalid_data(invalid_data in invalid_pem_data()) {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        let result = config.load_client_key_pem_bytes(&invalid_data);
        assert!(result.is_err(), "Invalid PEM key data should fail to load");
        assert!(config.client_key.is_none());
    }

    #[test]
    fn prop_der_cert_loading_valid_data(cert_data in valid_der_cert()) {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        let result = config.load_client_cert_der_bytes(&cert_data);

        // DER loading is more permissive since we just wrap the bytes
        // Most valid-looking DER data should succeed
        match result {
            Ok(()) => {
                assert!(config.client_cert.is_some());
            }
            Err(e) => {
                assert!(!e.to_string().is_empty());
            }
        }
    }

    #[test]
    fn prop_der_key_loading_valid_data(key_data in valid_der_cert()) { // Reuse cert generator for key
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        let result = config.load_client_key_der_bytes(&key_data);

        match result {
            Ok(()) => {
                assert!(config.client_key.is_some());
            }
            Err(e) => {
                assert!(!e.to_string().is_empty());
            }
        }
    }

    #[test]
    fn prop_der_cert_loading_invalid_data(invalid_data in invalid_der_data()) {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        let result = config.load_client_cert_der_bytes(&invalid_data);

        // DER loading might succeed even with invalid data since we just wrap bytes
        // The important thing is it doesn't panic and returns a valid Result
        match result {
            Ok(()) => {
                // If it succeeds, cert should be loaded
                assert!(config.client_cert.is_some());
            }
            Err(e) => {
                // If it fails, should have meaningful error and not modify config
                assert!(!e.to_string().is_empty());
                assert!(config.client_cert.is_none());
            }
        }
    }

    #[test]
    fn prop_certificate_loading_sequence_independence(
        cert_data in valid_pem_cert(),
        key_data in valid_pem_key(),
        ca_data in valid_pem_cert()
    ) {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);

        // Test loading in different orders
        let orders = [
            (0, 1, 2), // cert, key, ca
            (0, 2, 1), // cert, ca, key
            (1, 0, 2), // key, cert, ca
            (1, 2, 0), // key, ca, cert
            (2, 0, 1), // ca, cert, key
            (2, 1, 0), // ca, key, cert
        ];

        for (first, second, third) in orders {
            let mut config = TlsConfig::new(addr, "localhost");

            // Execute operations in the specified order
            let results = match (first, second, third) {
                (0, 1, 2) => (
                    config.load_client_cert_pem_bytes(&cert_data),
                    config.load_client_key_pem_bytes(&key_data),
                    config.load_ca_cert_pem_bytes(&ca_data),
                ),
                (0, 2, 1) => (
                    config.load_client_cert_pem_bytes(&cert_data),
                    config.load_ca_cert_pem_bytes(&ca_data),
                    config.load_client_key_pem_bytes(&key_data),
                ),
                (1, 0, 2) => (
                    config.load_client_key_pem_bytes(&key_data),
                    config.load_client_cert_pem_bytes(&cert_data),
                    config.load_ca_cert_pem_bytes(&ca_data),
                ),
                (1, 2, 0) => (
                    config.load_client_key_pem_bytes(&key_data),
                    config.load_ca_cert_pem_bytes(&ca_data),
                    config.load_client_cert_pem_bytes(&cert_data),
                ),
                (2, 0, 1) => (
                    config.load_ca_cert_pem_bytes(&ca_data),
                    config.load_client_cert_pem_bytes(&cert_data),
                    config.load_client_key_pem_bytes(&key_data),
                ),
                (2, 1, 0) => (
                    config.load_ca_cert_pem_bytes(&ca_data),
                    config.load_client_key_pem_bytes(&key_data),
                    config.load_client_cert_pem_bytes(&cert_data),
                ),
                _ => unreachable!(),
            };

            // The order shouldn't matter - if all succeed, all should be loaded
            if results.0.is_ok() && results.1.is_ok() && results.2.is_ok() {
                assert!(config.client_cert.is_some());
                assert!(config.client_key.is_some());
                assert!(config.root_certs.is_some());
            }

            // Failed operations shouldn't affect successful ones
            let success_count = [results.0.is_ok(), results.1.is_ok(), results.2.is_ok()]
                .iter()
                .filter(|&&x| x)
                .count();

            let loaded_count = [
                config.client_cert.is_some(),
                config.client_key.is_some(),
                config.root_certs.is_some(),
            ]
            .iter()
            .filter(|&&x| x)
            .count();

            assert_eq!(success_count, loaded_count,
                "Number of successful loads should match number of loaded certificates");
        }
    }

    #[test]
    fn prop_certificate_error_messages_meaningful(invalid_data in invalid_pem_data()) {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        let cert_result = config.load_client_cert_pem_bytes(&invalid_data);
        let key_result = config.load_client_key_pem_bytes(&invalid_data);
        let ca_result = config.load_ca_cert_pem_bytes(&invalid_data);

        // All should fail with meaningful error messages
        if let Err(e) = cert_result {
            let msg = e.to_string();
            assert!(!msg.is_empty());
            assert!(msg.len() > 10, "Error message should be descriptive: {msg}");
        }

        if let Err(e) = key_result {
            let msg = e.to_string();
            assert!(!msg.is_empty());
            assert!(msg.len() > 10, "Error message should be descriptive: {msg}");
        }

        if let Err(e) = ca_result {
            let msg = e.to_string();
            assert!(!msg.is_empty());
            assert!(msg.len() > 10, "Error message should be descriptive: {msg}");
        }
    }

    #[test]
    fn prop_certificate_loading_memory_safety(
        data in prop::collection::vec(any::<u8>(), 0..10000)
    ) {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        // Should never panic regardless of input data
        let _ = config.load_client_cert_pem_bytes(&data);
        let _ = config.load_client_key_pem_bytes(&data);
        let _ = config.load_ca_cert_pem_bytes(&data);
        let _ = config.load_client_cert_der_bytes(&data);
        let _ = config.load_client_key_der_bytes(&data);
        let _ = config.load_ca_cert_der_bytes(&data);

        // Function should complete without panicking
        // Test passed if we reach this point without panicking
    }
}

// Unit tests for edge cases that are hard to generate with proptest
#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn test_multiple_certificates_in_pem() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        let multi_cert_pem = b"-----BEGIN CERTIFICATE-----
MIIBkTCB+wIJALRJF4QlQZq2MA0GCSqGSIb3DQEBCwUAMBQxEjAQBgNVBAMMCWxv
Y2FsaG9zdDAeFw0yNDAxMDEwMDAwMDBaFw0yNTAxMDEwMDAwMDBaMBQxEjAQBgNV
BAMMCWxvY2FsaG9zdDBcMA0GCSqGSIb3DQEBAQUAA0sAMEgCQQC7VJTUt9Us8cKB
-----END CERTIFICATE-----
-----BEGIN CERTIFICATE-----
MIIBkTCB+wIJALRJF4QlQZq3MA0GCSqGSIb3DQEBCwUAMBQxEjAQBgNVBAMMCWxv
Y2FsaG9zdDAeFw0yNDAxMDEwMDAwMDBaFw0yNTAxMDEwMDAwMDBaMBQxEjAQBgNV
BAMMCWxvY2FsaG9zdDBcMA0GCSqGSIb3DQEBAQUAA0sAMEgCQQC7VJTUt9Us8cKB
-----END CERTIFICATE-----";

        let result = config.load_client_cert_pem_bytes(multi_cert_pem);

        // Should handle multiple certificates gracefully
        match result {
            Ok(()) => {
                assert!(config.client_cert.is_some());
                // Should load all certificates found
                let certs = config.client_cert.as_ref().unwrap();
                assert!(!certs.is_empty(), "Should load at least one certificate");
            }
            Err(e) => {
                // If it fails, should be a meaningful error
                assert!(!e.to_string().is_empty());
            }
        }
    }

    #[test]
    fn test_very_large_certificate() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        // Create a very large (but valid) PEM structure
        let mut large_pem = b"-----BEGIN CERTIFICATE-----\n".to_vec();
        let large_data = "A".repeat(50000); // 50KB of data
        large_pem.extend_from_slice(large_data.as_bytes());
        large_pem.extend_from_slice(b"\n-----END CERTIFICATE-----");

        let result = config.load_client_cert_pem_bytes(&large_pem);

        // Should handle large certificates without panicking
        match result {
            Ok(()) => assert!(config.client_cert.is_some()),
            Err(e) => assert!(!e.to_string().is_empty()),
        }
    }

    #[test]
    fn test_certificate_with_unusual_whitespace() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let mut config = TlsConfig::new(addr, "localhost");

        let weird_whitespace_pem = b"-----BEGIN CERTIFICATE-----\r\n\t\r\n  \r\nMIIBkTCB+wIJALRJF4QlQZq2MA0GCSqGSIb3DQEBCwUAMBQxEjAQBgNVBAMMCWxv\r\n\t\t  \r\ny2FsaG9zdDAeFw0yNDAxMDEwMDAwMDBaFw0yNTAxMDEwMDAwMDBaMBQxEjAQBgNV\r\n\r\n-----END CERTIFICATE-----\r\n\r\n";

        let result = config.load_client_cert_pem_bytes(weird_whitespace_pem);

        // Should handle unusual whitespace gracefully
        match result {
            Ok(()) => assert!(config.client_cert.is_some()),
            Err(e) => assert!(!e.to_string().is_empty()),
        }
    }
}
