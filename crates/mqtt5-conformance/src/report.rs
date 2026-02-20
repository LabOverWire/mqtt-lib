//! Conformance coverage report generation.
//!
//! Produces human-readable text reports and machine-readable JSON from a
//! [`ConformanceManifest`], showing which normative statements are tested.

#![allow(clippy::missing_panics_doc)]

use crate::manifest::{ConformanceManifest, TestStatus};
use std::fmt::Write;

/// Generates conformance coverage reports from a [`ConformanceManifest`].
pub struct ConformanceReport {
    manifest: ConformanceManifest,
}

impl ConformanceReport {
    #[must_use]
    pub fn new(manifest: ConformanceManifest) -> Self {
        Self { manifest }
    }

    /// Generates a human-readable text report with per-section coverage
    /// and a status marker for each normative statement.
    #[must_use]
    pub fn generate_text(&self) -> String {
        let mut out = String::new();
        out.push_str("MQTT v5.0 Conformance Report\n");
        out.push_str(&"=".repeat(60));
        out.push('\n');

        let total = self.manifest.total_statements();
        let tested = self.manifest.tested_count();
        let _ = writeln!(
            out,
            "Coverage: {tested}/{total} ({:.1}%)\n",
            self.manifest.coverage_percentage()
        );

        for (section_id, section) in &self.manifest.sections {
            let section_tested = section
                .statements
                .iter()
                .filter(|s| matches!(s.status, TestStatus::Tested))
                .count();
            let _ = writeln!(
                out,
                "Section {section_id}: {} ({section_tested}/{} tested)",
                section.title,
                section.statements.len()
            );

            for stmt in &section.statements {
                let marker = match stmt.status {
                    TestStatus::Tested => "[PASS]",
                    TestStatus::Untested => "[    ]",
                    TestStatus::NotApplicable => "[ NA ]",
                    TestStatus::Partial => "[PART]",
                    TestStatus::CrossRef => "[XREF]",
                    TestStatus::Skipped => "[SKIP]",
                };
                let text = truncate(&stmt.text, 70);
                let _ = writeln!(out, "  {marker} {} ({:?}): {text}", stmt.id, stmt.level);
            }
            out.push('\n');
        }
        out
    }

    /// Generates a machine-readable JSON representation of the manifest.
    #[must_use]
    pub fn generate_json(&self) -> String {
        serde_json::to_string_pretty(&self.manifest).expect("json serialization failed")
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut result: String = s.chars().take(max).collect();
        result.push_str("...");
        result
    }
}
