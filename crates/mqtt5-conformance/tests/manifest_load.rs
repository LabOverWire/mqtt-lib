use mqtt5_conformance::manifest::{ConformanceManifest, TestStatus};
use mqtt5_conformance::registry::CONFORMANCE_TESTS;
use std::collections::BTreeSet;

fn spec_statement_ids() -> BTreeSet<String> {
    include_str!("../mqtt-v5.0-statement-ids.txt")
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("MQTT-"))
        .map(ToString::to_string)
        .collect()
}

fn manifest_statement_ids(manifest: &ConformanceManifest) -> BTreeSet<String> {
    manifest
        .sections
        .values()
        .flat_map(|s| &s.statements)
        .map(|st| st.id.clone())
        .collect()
}

#[test]
fn every_manifest_id_is_a_real_v5_statement() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let spec = spec_statement_ids();
    let invented: Vec<_> = manifest_statement_ids(&manifest)
        .into_iter()
        .filter(|id| !spec.contains(id))
        .collect();
    assert!(
        invented.is_empty(),
        "manifest declares statement IDs that do not exist in OASIS MQTT v5.0: {invented:?}"
    );
}

#[test]
fn every_v5_statement_is_present_in_the_manifest() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let present = manifest_statement_ids(&manifest);
    let missing: Vec<_> = spec_statement_ids()
        .into_iter()
        .filter(|id| !present.contains(id))
        .collect();
    assert!(
        missing.is_empty(),
        "OASIS MQTT v5.0 statements absent from the manifest: {missing:?}"
    );
}

#[test]
fn no_statement_id_is_declared_twice() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let mut seen = BTreeSet::new();
    let mut duplicates = Vec::new();
    for statement in manifest.sections.values().flat_map(|s| &s.statements) {
        if !seen.insert(statement.id.clone()) {
            duplicates.push(statement.id.clone());
        }
    }
    assert!(
        duplicates.is_empty(),
        "duplicate statement IDs: {duplicates:?}"
    );
}

#[test]
fn every_cited_test_is_registered() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let registered: BTreeSet<&str> = CONFORMANCE_TESTS.iter().map(|t| t.name).collect();
    let mut dangling = Vec::new();
    for statement in manifest.sections.values().flat_map(|s| &s.statements) {
        for name in &statement.test_names {
            if !registered.contains(name.as_str()) {
                dangling.push(format!("{} -> {name}", statement.id));
            }
        }
    }
    assert!(
        dangling.is_empty(),
        "manifest cites tests that are not in the conformance registry, so they never run: {dangling:?}"
    );
}

#[test]
fn every_registered_test_targets_a_known_statement() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let known = manifest_statement_ids(&manifest);
    let mut stray = Vec::new();
    for test in CONFORMANCE_TESTS {
        for id in test.ids {
            if !known.contains(*id) {
                stray.push(format!("{} -> {id}", test.name));
            }
        }
    }
    assert!(
        stray.is_empty(),
        "registered tests declare statement IDs the manifest does not track: {stray:?}"
    );
}

#[test]
fn tested_statements_cite_at_least_one_test() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let unsupported: Vec<_> = manifest
        .sections
        .values()
        .flat_map(|s| &s.statements)
        .filter(|st| matches!(st.status, TestStatus::Tested) && st.test_names.is_empty())
        .map(|st| st.id.clone())
        .collect();
    assert!(
        unsupported.is_empty(),
        "statements marked Tested with no test_names: {unsupported:?}"
    );
}

fn spec_statement_texts() -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut current: Option<String> = None;
    for line in include_str!("../mqtt-v5.0-statement-texts.txt").lines() {
        if let Some(id) = line.strip_prefix("## ") {
            current = Some(id.trim().to_string());
        } else if let Some(id) = current.clone() {
            if !line.trim().is_empty() {
                out.entry(id).or_insert_with(|| line.trim().to_string());
            }
        }
    }
    out
}

fn normalise(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn known_text_drift() -> BTreeSet<String> {
    include_str!("../known-text-drift.txt")
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("MQTT-"))
        .map(ToString::to_string)
        .collect()
}

#[test]
fn statement_text_drift_only_shrinks() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let spec = spec_statement_texts();
    let mut drifted = BTreeSet::new();
    for statement in manifest.sections.values().flat_map(|s| &s.statements) {
        let Some(reference) = spec.get(&statement.id) else {
            continue;
        };
        let ours = normalise(&statement.text);
        let theirs = normalise(reference);
        if !theirs.contains(&ours) && !ours.contains(&theirs) {
            drifted.insert(statement.id.clone());
        }
    }
    let known = known_text_drift();
    let fresh: Vec<_> = drifted.difference(&known).cloned().collect();
    assert!(
        fresh.is_empty(),
        "statement text does not correspond to the OASIS statement of that ID, and is not recorded \
         in known-text-drift.txt: {fresh:?}"
    );
    let repaired: Vec<_> = known.difference(&drifted).cloned().collect();
    assert!(
        repaired.is_empty(),
        "these statements no longer drift and must be removed from known-text-drift.txt: {repaired:?}"
    );
}

fn known_citation_drift() -> BTreeSet<String> {
    include_str!("../known-citation-drift.txt")
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(|c: char| c.is_ascii_lowercase()) && l.contains(" MQTT-"))
        .map(ToString::to_string)
        .collect()
}

#[test]
fn manifest_and_test_attributes_agree_or_shrink() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let mut disagreements = BTreeSet::new();
    for test in CONFORMANCE_TESTS {
        for id in test.ids {
            let cited = manifest
                .sections
                .values()
                .flat_map(|s| &s.statements)
                .find(|st| st.id == *id)
                .is_some_and(|st| st.test_names.iter().any(|n| n == test.name));
            if !cited {
                disagreements.insert(format!("{} {id}", test.name));
            }
        }
    }
    let known = known_citation_drift();
    let fresh: Vec<_> = disagreements.difference(&known).cloned().collect();
    assert!(
        fresh.is_empty(),
        "a registered test declares a statement that does not cite it back, and the pair is not \
         recorded in known-citation-drift.txt: {fresh:?}"
    );
    let repaired: Vec<_> = known.difference(&disagreements).cloned().collect();
    assert!(
        repaired.is_empty(),
        "these pairs are now reconciled and must be removed from known-citation-drift.txt: {repaired:?}"
    );
}

#[test]
fn section_totals_match_statement_counts() {
    let manifest = ConformanceManifest::load("conformance.toml");
    let missing: Vec<_> = manifest
        .sections
        .iter()
        .filter(|(_, s)| s.total_statements.is_none())
        .map(|(k, _)| k.clone())
        .collect();
    assert!(
        missing.is_empty(),
        "sections without a declared total_statements, which would silently disable this guard: {missing:?}"
    );
    let wrong: Vec<_> = manifest
        .sections
        .iter()
        .filter(|(_, s)| s.total_statements.is_some_and(|n| n != s.statements.len()))
        .map(|(k, s)| {
            format!(
                "{k}: declared {:?} but holds {}",
                s.total_statements,
                s.statements.len()
            )
        })
        .collect();
    assert!(wrong.is_empty(), "section totals out of sync: {wrong:?}");
}

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
