---
title: "mqtt5: A Rust MQTT v5.0 Client and Broker Library with QUIC Transport and Specification Conformance Testing"
tags:
  - Rust
  - MQTT
  - IoT
  - QUIC
  - WebAssembly
  - publish-subscribe
authors:
  - name: Fabrício Bracht
    orcid: 0000-0002-4758-0159
    corresponding: true
    affiliation: 1
affiliations:
  - name: LabOverWire
    index: 1
date: 9 March 2026
bibliography: paper.bib
---

# Summary

`mqtt5` is an open-source Rust library providing a complete MQTT v5.0 [@mqtt-v5-spec] client and broker across four transports (TCP, TLS 1.3, WebSocket, and QUIC) and three platform targets (native, WebAssembly, and bare-metal embedded). Its QUIC transport, built on Quinn [@quinn], lets users choose how MQTT topics map onto QUIC streams and optionally carry messages over unreliable datagrams. A companion conformance suite tracks every normative statement in the OASIS specification and links each one to the test functions that verify it. The library is organized as a Cargo workspace of five crates totaling approximately 47,000 lines of code and 33,000 lines of tests.

# Statement of need

MQTT v5.0 is a widely adopted publish-subscribe protocol for IoT deployments [@bansal2020iot-mqtt-survey], yet studying how it interacts with modern transport-layer features requires tooling that existing implementations do not provide.

QUIC multiplexes independent byte streams within a single connection, which in principle eliminates head-of-line blocking between unrelated data flows. Whether this advantage materializes for MQTT traffic depends on how topics are mapped onto streams: a single shared stream behaves like TCP; one stream per topic isolates retransmission delays between topics; one stream per message provides maximum independence at higher per-message overhead. Exploring these trade-offs today means modifying broker internals or writing a custom stack, because no published MQTT library exposes stream mapping as a user-facing option. `mqtt5` turns this into a runtime configuration choice, and adds an unreliable datagram mode for loss-tolerant use cases.

Separately, the OASIS MQTT v5.0 standard contains 247 normative statements identified by a machine-readable pattern (`[MQTT-x.x.x-y]`), yet no published implementation maps each statement to a test function. Without per-statement traceability, conformance gaps are discovered ad hoc rather than systematically. `mqtt5` maintains a manifest (`conformance.toml`) that classifies every statement and links it to the tests that verify it, providing a reusable methodology for protocol conformance tracking.

Finally, MQTT deployments span cloud servers, browsers, and microcontrollers, but existing libraries target only a subset of these environments. The `no_std` protocol codec in `mqtt5` enables use on ARM Cortex-M4 and RISC-V devices, while the WASM crate provides JavaScript bindings for browser-based MQTT clients and an optional in-browser broker capable of bridging to external brokers.

# State of the field

Mosquitto [@mosquitto] is a widely deployed C broker and client supporting TCP, TLS, and WebSocket. EMQX [@emqx] is an Erlang/OTP broker designed for horizontal scaling; its QUIC support uses a fixed single-stream mapping. HiveMQ [@hivemq] is a Java broker with an open-source community edition and proprietary enterprise features. rumqtt [@rumqtt] is a Rust client and broker with partial MQTT v5.0 support over TCP, TLS, and WebSocket. paho-mqtt [@paho-mqtt] is an Eclipse Foundation client library available in C, C++, Java, Python, and other languages, supporting v5.0 over TCP, TLS, and WebSocket.

None of these projects offer user-selectable QUIC stream mapping strategies, a datagram transport mode, a conformance suite with per-statement traceability, or compilation from a single codebase to native, WebAssembly, and bare-metal embedded targets.

# Software design

The library separates concerns across five crates. `mqtt5-protocol` implements the packet codec, topic validation, session state, and QoS flow control under `no_std` with the `alloc` crate, enabling bare-metal deployment. `mqtt5` adds async networking via Tokio [@tokio], transport implementations, the broker's message router, authentication, and persistent storage. `mqtt5-wasm` wraps the client and broker with `wasm-bindgen` for browser environments. `mqttv5-cli` provides a command-line tool for publishing, subscribing, broker operation, and benchmarking. `mqtt5-conformance` contains the specification test suite.

All transports implement a common `Transport` trait, and the broker can listen on all four simultaneously. The QUIC stream mapping—control-only, per-topic, or per-publish—is selected at connection time without code changes; the datagram mode is toggled independently.

The broker comprises a message router with exact-match and wildcard subscription tables supporting shared subscriptions; session management with pluggable storage backends (in-memory or file-based with atomic writes); a pluggable authentication system covering passwords, SCRAM-SHA-256, JWT, and X.509 certificates; and a security layer that injects verifiable sender identity properties on every published message.

The conformance suite operates in three tiers: raw protocol tests sending hand-crafted byte sequences to probe malformed-packet handling, structural tests inspecting response packet fields, and behavioral tests exercising end-to-end message flows through the library's own client. This process exposed 18 implementation gaps—9 missing validations and 9 incorrect behaviors—documented in a development diary with normative statement references and fixing commits.

Beyond conformance, the library is verified through 483 integration tests, property-based testing with proptest [@proptest] for topic validation and session state invariants, and deterministic network simulation with Turmoil [@turmoil] for partition and latency testing. Downstream consumers—a web frontend importing the WASM npm package, and the CLI tool—serve as implicit integration harnesses that surface defects at target boundaries. The CI pipeline runs seven parallel jobs: formatting, pedantic Clippy linting, unit tests, MSRV verification, documentation, WASM compilation, and embedded cross-compilation for ARM Cortex-M4 and RISC-V.

# Research impact statement

The library's built-in benchmarking tool (`mqttv5 bench`) has been used in a series of controlled experiments comparing TCP and QUIC under emulated network impairment. Because the only variable between runs is the stream mapping strategy, confounds from differing MQTT implementations are eliminated. Experiments conducted so far cover connection latency, head-of-line blocking isolation, throughput under packet loss, datagram-versus-stream trade-offs, and resource overhead at scale.

The conformance methodology is reusable beyond MQTT: any protocol with machine-readable normative identifiers can adopt the same manifest-driven approach. The 18 gaps discovered during development demonstrate the practical value of systematic per-statement testing.

The library is published on crates.io as the `mqtt5` crate and as an npm package for browser-based use.

# AI usage disclosure

Development used Claude Code (Anthropic) as a code generation and automation tool operating under developer direction. The developer drove all architectural decisions, reviewed all generated code, performed quality assurance, and analyzed defects. The AI tool executed specific tasks on command: writing and rewriting code, running build and test commands, writing automation scripts, and editing text. All AI-generated code passed through the same CI pipeline, conformance verification, and pedantic linting as manually written code. This paper was drafted with AI assistance and reviewed and edited by the author.

# Acknowledgements

The QUIC transport implementation uses Quinn [@quinn], and the async runtime is provided by Tokio [@tokio].

# References
