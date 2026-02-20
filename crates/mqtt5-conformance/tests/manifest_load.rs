use mqtt5_conformance::manifest::{ConformanceManifest, TestStatus};

#[test]
fn manifest_loads_and_parses_all_sections() {
    let manifest = ConformanceManifest::load("conformance.toml");

    assert!(
        manifest.total_statements() > 200,
        "expected 200+ statements, got {}",
        manifest.total_statements()
    );
    assert!(
        manifest.tested_count() > 0,
        "expected at least one tested statement"
    );

    let cross_ref_count = manifest
        .sections
        .values()
        .flat_map(|s| &s.statements)
        .filter(|st| matches!(st.status, TestStatus::CrossRef))
        .count();
    assert!(
        cross_ref_count > 0,
        "expected at least one CrossRef statement"
    );
}

#[test]
fn manifest_report_round_trip() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let report = mqtt5_conformance::report::ConformanceReport::new(manifest);
    let text = report.generate_text();
    assert!(text.contains("MQTT v5.0 Conformance Report"));
    assert!(text.contains("[XREF]"));

    let json = report.generate_json();
    let _: serde_json::Value = serde_json::from_str(&json).expect("valid JSON output");
}
