//! Compile-time registry of conformance tests.
//!
//! Tests annotated with `#[mqtt5_conformance_macros::conformance_test]`
//! register themselves into the [`CONFORMANCE_TESTS`] distributed slice at
//! compile time. The CLI runner walks this slice to build a libtest-mimic
//! `Trial` for each test, evaluates the SUT capability requirements, and
//! either runs or marks the trial as skipped.
//!
//! The slice survives across crates: every crate that depends on
//! `mqtt5-conformance` and uses `#[conformance_test]` contributes entries.
//! The runner observes the union without any explicit registration step.

use crate::capabilities::Requirement;
use crate::sut::SutHandle;
use std::future::Future;
use std::pin::Pin;

pub use linkme;

/// A future returned by a conformance-test runner closure.
///
/// `'static` so the test can be queued onto a tokio runtime by the CLI
/// runner without lifetime gymnastics.
pub type TestFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Type-erased entry-point a registered test exposes to the runner.
///
/// The runner provides the constructed [`SutHandle`] (constructed once per
/// process for the in-process fixture, or shared for an external SUT) and
/// receives a future that drives the test body to completion. The future
/// panics on assertion failure, matching libtest-mimic's expectations.
pub type TestRunner = fn(SutHandle) -> TestFuture;

/// A single conformance test registered at compile time.
///
/// Constructed by the `#[conformance_test]` proc-macro and stored in the
/// [`CONFORMANCE_TESTS`] distributed slice. The runner reads these records
/// to build the test plan; it never instantiates them by hand.
#[derive(Debug, Clone, Copy)]
pub struct ConformanceTest {
    pub name: &'static str,
    pub module_path: &'static str,
    pub file: &'static str,
    pub line: u32,
    pub ids: &'static [&'static str],
    pub requires: &'static [Requirement],
    pub runner: TestRunner,
}

/// Distributed slice collecting every `#[conformance_test]` in the build.
///
/// `linkme` arranges for the linker to concatenate per-test entries into
/// this slot at link time, with no startup-time registration cost. The CLI
/// runner iterates this slice to build the test plan.
#[linkme::distributed_slice]
pub static CONFORMANCE_TESTS: [ConformanceTest] = [..];
