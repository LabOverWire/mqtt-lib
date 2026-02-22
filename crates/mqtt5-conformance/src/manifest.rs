//! Structured conformance manifest for tracking MQTT v5.0 normative statements.
//!
//! The manifest is a TOML file (`conformance.toml`) that maps every normative
//! statement from the OASIS spec to its test status and associated test names.
//! Use [`ConformanceManifest::load`] to deserialize and query coverage metrics.

#![allow(clippy::missing_panics_doc, clippy::cast_precision_loss)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Root manifest containing all spec sections and their normative statements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceManifest {
    pub sections: BTreeMap<String, Section>,
}

/// A section of the MQTT v5.0 specification (e.g. "3.1" for CONNECT).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub title: String,
    pub total_statements: Option<usize>,
    pub note: Option<String>,
    #[serde(default)]
    pub statements: Vec<NormativeStatement>,
}

/// A single normative statement from the OASIS spec, identified by its
/// `[MQTT-x.x.x-y]` conformance ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormativeStatement {
    pub id: String,
    pub level: Level,
    pub applies_to: AppliesTo,
    pub text: String,
    pub status: TestStatus,
    pub test_names: Vec<String>,
    pub note: Option<String>,
}

/// RFC 2119 requirement level of a normative statement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Level {
    Must,
    MustNot,
    Should,
    ShouldNot,
    May,
}

/// Whether a normative statement applies to the server, client, or both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppliesTo {
    Server,
    Client,
    Both,
}

/// Current test coverage status for a normative statement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TestStatus {
    Tested,
    Untested,
    NotApplicable,
    Partial,
    CrossRef,
    Skipped,
}

impl ConformanceManifest {
    /// Loads and deserializes a conformance manifest from a TOML file.
    #[must_use]
    pub fn load(path: &str) -> Self {
        let content = std::fs::read_to_string(path).expect("conformance manifest not found");
        toml::from_str(&content).expect("invalid conformance manifest")
    }

    /// Returns the total number of normative statements across all sections.
    #[must_use]
    pub fn total_statements(&self) -> usize {
        self.sections.values().map(|s| s.statements.len()).sum()
    }

    /// Returns the number of statements with [`TestStatus::Tested`].
    #[must_use]
    pub fn tested_count(&self) -> usize {
        self.sections
            .values()
            .flat_map(|s| &s.statements)
            .filter(|st| matches!(st.status, TestStatus::Tested))
            .count()
    }

    /// Returns the number of statements with [`TestStatus::Untested`].
    #[must_use]
    pub fn untested_count(&self) -> usize {
        self.sections
            .values()
            .flat_map(|s| &s.statements)
            .filter(|st| matches!(st.status, TestStatus::Untested))
            .count()
    }

    /// Returns the number of statements that apply only to the server.
    #[must_use]
    pub fn server_only_count(&self) -> usize {
        self.sections
            .values()
            .flat_map(|s| &s.statements)
            .filter(|st| matches!(st.applies_to, AppliesTo::Server))
            .count()
    }

    /// Returns the percentage of statements that are tested.
    #[must_use]
    pub fn coverage_percentage(&self) -> f64 {
        let total = self.total_statements();
        if total == 0 {
            return 0.0;
        }
        (self.tested_count() as f64 / total as f64) * 100.0
    }
}
