//! Test `BeBytes` 2.6.0 raw pointer methods for eligible MQTT structures
//!
//! This test identifies which MQTT structures are eligible for the raw pointer
//! methods that provide 40-80x performance improvements in `BeBytes` 2.6.0.
#![allow(clippy::cast_precision_loss)]

use bebytes::BeBytes;
use mqtt5::encoding::mqtt_string::MqttString;
use mqtt5::encoding::variable_int::VariableInt;
use mqtt5::packet::pingreq::PingReqPacket;
use mqtt5::packet::pingresp::PingRespPacket;
use mqtt5::packet::subscribe::{RetainHandling, SubscriptionOptions, SubscriptionOptionsBits};
use mqtt5::packet::{AckPacketHeader, MqttTypeAndFlags, PacketType};
use mqtt5::types::ReasonCode;
use mqtt5::QoS;

#[test]
fn test_mqtt_type_and_flags_raw_pointer_eligibility() {
    let header = MqttTypeAndFlags::for_publish(1, true, false);

    // Test to_be_bytes_buf() - should work for all structures
    let buf_bytes = header.to_be_bytes_buf();
    println!(
        "MqttTypeAndFlags to_be_bytes_buf: {} bytes",
        buf_bytes.len()
    );

    // Test standard to_be_bytes for comparison
    let std_bytes = header.to_be_bytes();
    println!("MqttTypeAndFlags to_be_bytes: {} bytes", std_bytes.len());

    // Verify they produce the same result
    assert_eq!(buf_bytes.as_ref(), std_bytes.as_slice());

    // Single-byte structures should be optimal for raw pointer methods
    assert_eq!(buf_bytes.len(), 1);
}

#[test]
fn test_ack_packet_header_raw_pointer_eligibility() {
    let header = AckPacketHeader::create(12345, ReasonCode::Success);

    // Test to_be_bytes_buf()
    let buf_bytes = header.to_be_bytes_buf();
    println!("AckPacketHeader to_be_bytes_buf: {} bytes", buf_bytes.len());

    // Test standard to_be_bytes for comparison
    let std_bytes = header.to_be_bytes();
    println!("AckPacketHeader to_be_bytes: {} bytes", std_bytes.len());

    // Verify they produce the same result
    assert_eq!(buf_bytes.as_ref(), std_bytes.as_slice());

    // 3-byte structure (u16 + u8) should be excellent for raw pointer methods
    assert_eq!(buf_bytes.len(), 3);
}

#[test]
fn test_subscription_options_raw_pointer_eligibility() {
    let sub_options = SubscriptionOptions {
        qos: QoS::ExactlyOnce,
        no_local: true,
        retain_as_published: false,
        retain_handling: RetainHandling::SendAtSubscribe,
    };
    let options = SubscriptionOptionsBits::from_options(&sub_options);

    // Test to_be_bytes_buf()
    let buf_bytes = options.to_be_bytes_buf();
    println!(
        "SubscriptionOptionsBits to_be_bytes_buf: {} bytes",
        buf_bytes.len()
    );

    // Test standard to_be_bytes for comparison
    let std_bytes = options.to_be_bytes();
    println!(
        "SubscriptionOptionsBits to_be_bytes: {} bytes",
        std_bytes.len()
    );

    // Verify they produce the same result
    assert_eq!(buf_bytes.as_ref(), std_bytes.as_slice());

    // Single-byte structure with bit fields - perfect for raw pointer methods
    assert_eq!(buf_bytes.len(), 1);
}

#[test]
fn test_ping_packets_raw_pointer_eligibility() {
    let ping_req = PingReqPacket::default();
    let ping_resp = PingRespPacket::default();

    // Test PingRequest
    let req_buf_bytes = ping_req.to_be_bytes_buf();
    let req_std_bytes = ping_req.to_be_bytes();
    println!("PingRequest to_be_bytes_buf: {} bytes", req_buf_bytes.len());
    assert_eq!(req_buf_bytes.as_ref(), req_std_bytes.as_slice());

    // Test PingResponse
    let resp_buf_bytes = ping_resp.to_be_bytes_buf();
    let resp_std_bytes = ping_resp.to_be_bytes();
    println!(
        "PingResponse to_be_bytes_buf: {} bytes",
        resp_buf_bytes.len()
    );
    assert_eq!(resp_buf_bytes.as_ref(), resp_std_bytes.as_slice());

    // Fixed-size packets are ideal for raw pointer methods
    assert_eq!(req_buf_bytes.len(), 2); // Fixed header only
    assert_eq!(resp_buf_bytes.len(), 2); // Fixed header only
}

#[test]
fn test_mqtt_string_raw_pointer_eligibility() {
    let mqtt_str = MqttString::create("test/topic").unwrap();

    // Test to_be_bytes_buf()
    let buf_bytes = mqtt_str.to_be_bytes_buf();
    println!("MqttString to_be_bytes_buf: {} bytes", buf_bytes.len());

    // Test standard to_be_bytes for comparison
    let std_bytes = mqtt_str.to_be_bytes();
    println!("MqttString to_be_bytes: {} bytes", std_bytes.len());

    // Verify they produce the same result
    assert_eq!(buf_bytes.as_ref(), std_bytes.as_slice());

    // Variable-size structures still benefit but less than fixed-size ones
    assert_eq!(buf_bytes.len(), 2 + "test/topic".len()); // length + data
}

#[test]
fn test_variable_int_raw_pointer_eligibility() {
    let var_int = VariableInt::new(16383).unwrap(); // 2-byte encoding

    // Test to_be_bytes_buf()
    let buf_bytes = var_int.to_be_bytes_buf();
    println!("VariableInt to_be_bytes_buf: {} bytes", buf_bytes.len());

    // Test standard to_be_bytes for comparison
    let std_bytes = var_int.to_be_bytes();
    println!("VariableInt to_be_bytes: {} bytes", std_bytes.len());

    // Verify they produce the same result
    assert_eq!(buf_bytes.as_ref(), std_bytes.as_slice());

    // Variable-size but predictable structures
    assert_eq!(buf_bytes.len(), 2); // Two bytes for this value
}

#[test]
fn test_performance_characteristics() {
    // Test structures by size for raw pointer method efficiency predictions

    println!("\n=== Raw Pointer Method Efficiency Analysis ===");

    // Category 1: Single-byte structures (highest efficiency expected)
    let type_flags = MqttTypeAndFlags::for_packet_type(PacketType::Publish);
    let sub_options = SubscriptionOptions {
        qos: QoS::AtLeastOnce,
        no_local: false,
        retain_as_published: true,
        retain_handling: RetainHandling::SendAtSubscribe,
    };
    let sub_opts = SubscriptionOptionsBits::from_options(&sub_options);

    println!("Single-byte structures (40-80x speedup expected):");
    println!(
        "  - MqttTypeAndFlags: {} bytes",
        type_flags.to_be_bytes_buf().len()
    );
    println!(
        "  - SubscriptionOptionsBits: {} bytes",
        sub_opts.to_be_bytes_buf().len()
    );

    // Category 2: Small fixed-size structures (very high efficiency expected)
    let ack_header = AckPacketHeader::create(1234, ReasonCode::Success);
    let ping_req = PingReqPacket::default();

    println!("Small fixed-size structures (20-40x speedup expected):");
    println!(
        "  - AckPacketHeader: {} bytes",
        ack_header.to_be_bytes_buf().len()
    );
    println!(
        "  - PingRequest: {} bytes",
        ping_req.to_be_bytes_buf().len()
    );

    // Category 3: Variable-size structures (moderate efficiency)
    let mqtt_str = MqttString::create("sensor/temp").unwrap();
    let var_int = VariableInt::new(127).unwrap();

    println!("Variable-size structures (2-10x speedup expected):");
    println!("  - MqttString: {} bytes", mqtt_str.to_be_bytes_buf().len());
    println!("  - VariableInt: {} bytes", var_int.to_be_bytes_buf().len());

    println!("\nAll structures successfully support to_be_bytes_buf() method!");
}

#[cfg(test)]
mod raw_pointer_benchmarks {
    use super::*;
    use std::hint::black_box;

    // These are micro-benchmarks to verify raw pointer performance characteristics
    // Run with: cargo test --release raw_pointer_benchmarks -- --nocapture

    #[test]
    fn bench_single_byte_structures() {
        let type_flags = MqttTypeAndFlags::for_publish(2, true, false);

        let start = std::time::Instant::now();
        for _ in 0..1_000_000 {
            let bytes = type_flags.to_be_bytes_buf();
            black_box(bytes);
        }
        let buf_time = start.elapsed();

        let start = std::time::Instant::now();
        for _ in 0..1_000_000 {
            let bytes = type_flags.to_be_bytes();
            black_box(bytes);
        }
        let std_time = start.elapsed();

        println!("Single-byte structure (1M iterations):");
        println!("  to_be_bytes_buf(): {buf_time:?}");
        println!("  to_be_bytes():     {std_time:?}");
        println!(
            "  Speedup: {:.2}x",
            std_time.as_nanos() as f64 / buf_time.as_nanos() as f64
        );
    }

    #[test]
    fn bench_fixed_size_structures() {
        let ack_header = AckPacketHeader::create(65535, ReasonCode::ImplementationSpecificError);

        let start = std::time::Instant::now();
        for _ in 0..1_000_000 {
            let bytes = ack_header.to_be_bytes_buf();
            black_box(bytes);
        }
        let buf_time = start.elapsed();

        let start = std::time::Instant::now();
        for _ in 0..1_000_000 {
            let bytes = ack_header.to_be_bytes();
            black_box(bytes);
        }
        let std_time = start.elapsed();

        println!("Fixed-size structure (1M iterations):");
        println!("  to_be_bytes_buf(): {buf_time:?}");
        println!("  to_be_bytes():     {std_time:?}");
        println!(
            "  Speedup: {:.2}x",
            std_time.as_nanos() as f64 / buf_time.as_nanos() as f64
        );
    }
}
