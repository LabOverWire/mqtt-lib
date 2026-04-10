//! CLI runner for the MQTT v5 conformance test suite.
//!
//! Walks the `mqtt5_conformance::registry::CONFORMANCE_TESTS` distributed
//! slice at startup and turns every registered test into a
//! [`libtest_mimic::Trial`]. Tests whose capability requirements are not
//! satisfied by the configured SUT are wrapped in an ignored trial so the
//! conclusion cleanly distinguishes pass / fail / skipped.
//!
//! ```text
//! mqtt5-conformance                              # in-process broker
//! mqtt5-conformance --sut external.toml          # external broker
//! mqtt5-conformance --sut external.toml --report report.json
//! ```
//!
//! Any unrecognised argument is forwarded to libtest-mimic, so `--list`,
//! `--ignored`, `--nocapture`, and substring filters behave exactly like
//! `cargo test` output.

#![warn(clippy::pedantic)]

use futures_util::FutureExt;
use libtest_mimic::{Arguments, Conclusion, Failed, Trial};
use mqtt5_conformance::{
    capabilities::{Capabilities, Requirement},
    registry::{ConformanceTest, CONFORMANCE_TESTS},
    sut::{SutDescriptor, SutHandle},
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use tokio::runtime::Runtime;

fn main() -> ExitCode {
    let (options, libtest_args) = CliOptions::from_env();

    let plan = match SutPlan::from_options(&options) {
        Ok(plan) => Arc::new(plan),
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };

    let runtime = match Runtime::new() {
        Ok(runtime) => Arc::new(runtime),
        Err(error) => {
            eprintln!("error: failed to start tokio runtime: {error}");
            return ExitCode::from(2);
        }
    };

    let capabilities = plan.capabilities_snapshot(&runtime);
    let trials = build_trials(&capabilities, &plan, &runtime);
    let conclusion = libtest_mimic::run(&libtest_args, trials);

    if let Some(report_path) = options.report.as_deref() {
        if let Err(error) = write_report(report_path, &capabilities, &conclusion) {
            eprintln!(
                "warning: failed to write report {}: {error}",
                report_path.display()
            );
        }
    }

    conclusion.exit_code()
}

struct CliOptions {
    sut: Option<PathBuf>,
    report: Option<PathBuf>,
}

impl CliOptions {
    fn from_env() -> (Self, Arguments) {
        let mut sut: Option<PathBuf> = None;
        let mut report: Option<PathBuf> = None;
        let mut forwarded: Vec<String> = Vec::new();

        let mut raw = std::env::args();
        let program = raw.next().unwrap_or_default();
        forwarded.push(program);

        while let Some(arg) = raw.next() {
            if let Some(value) = arg.strip_prefix("--sut=") {
                sut = Some(PathBuf::from(value));
            } else if arg == "--sut" {
                sut = raw.next().map(PathBuf::from);
            } else if let Some(value) = arg.strip_prefix("--report=") {
                report = Some(PathBuf::from(value));
            } else if arg == "--report" {
                report = raw.next().map(PathBuf::from);
            } else {
                forwarded.push(arg);
            }
        }

        let libtest = Arguments::from_iter(forwarded);
        (Self { sut, report }, libtest)
    }
}

enum SutPlan {
    External(Arc<SutDescriptor>),
    InProcess,
}

impl SutPlan {
    fn from_options(options: &CliOptions) -> Result<Self, String> {
        match options.sut.as_deref() {
            Some(path) => {
                let descriptor = SutDescriptor::from_file(path)
                    .map_err(|error| format!("failed to load {}: {error}", path.display()))?;
                Ok(Self::External(Arc::new(descriptor)))
            }
            None => Ok(Self::InProcess),
        }
    }

    fn capabilities_snapshot(&self, runtime: &Runtime) -> Capabilities {
        match self {
            Self::External(descriptor) => descriptor.capabilities.clone(),
            Self::InProcess => {
                let handle =
                    runtime.block_on(mqtt5_conformance::sut::inprocess_sut_with_websocket());
                handle.capabilities().clone()
            }
        }
    }

    fn build_handle(&self, runtime: &Runtime) -> SutHandle {
        match self {
            Self::External(descriptor) => SutHandle::External(Box::new((**descriptor).clone())),
            Self::InProcess => {
                runtime.block_on(mqtt5_conformance::sut::inprocess_sut_with_websocket())
            }
        }
    }
}

fn build_trials(
    capabilities: &Capabilities,
    plan: &Arc<SutPlan>,
    runtime: &Arc<Runtime>,
) -> Vec<Trial> {
    let mut trials = Vec::with_capacity(CONFORMANCE_TESTS.len());
    for test in CONFORMANCE_TESTS.iter().copied() {
        let name = trial_name(&test);
        let trial = if let Some(requirement) = first_unmet_requirement(capabilities, test.requires)
        {
            let skip_label = format!("{name} [missing: {}]", requirement.label());
            Trial::test(skip_label, || Ok(())).with_ignored_flag(true)
        } else {
            let plan = Arc::clone(plan);
            let runtime = Arc::clone(runtime);
            Trial::test(name, move || run_trial(&plan, &runtime, &test))
        };
        trials.push(trial);
    }
    trials
}

fn trial_name(test: &ConformanceTest) -> String {
    match test.ids.first() {
        Some(primary) => format!("{}::{} [{primary}]", test.module_path, test.name),
        None => format!("{}::{}", test.module_path, test.name),
    }
}

fn first_unmet_requirement(
    capabilities: &Capabilities,
    requirements: &[Requirement],
) -> Option<Requirement> {
    requirements
        .iter()
        .copied()
        .find(|requirement| !requirement.matches(capabilities))
}

fn run_trial(plan: &SutPlan, runtime: &Runtime, test: &ConformanceTest) -> Result<(), Failed> {
    let handle = plan.build_handle(runtime);
    let test = *test;
    let future = (test.runner)(handle);
    let outcome: Result<(), Box<dyn std::any::Any + Send>> = runtime.block_on(async {
        let result = std::panic::AssertUnwindSafe(future).catch_unwind().await;
        tokio::task::yield_now().await;
        result
    });
    match outcome {
        Ok(()) => Ok(()),
        Err(payload) => Err(Failed::from(format!(
            "{}: {}",
            test.name,
            panic_message(payload.as_ref())
        ))),
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic>".to_owned()
    }
}

fn write_report(
    path: &Path,
    capabilities: &Capabilities,
    conclusion: &Conclusion,
) -> std::io::Result<()> {
    let tests_by_id = collect_test_results(capabilities);
    let report = serde_json::json!({
        "summary": {
            "passed": conclusion.num_passed,
            "failed": conclusion.num_failed,
            "ignored": conclusion.num_ignored,
            "filtered_out": conclusion.num_filtered_out,
            "measured": conclusion.num_measured,
        },
        "capabilities": {
            "max_qos": capabilities.max_qos,
            "retain_available": capabilities.retain_available,
            "wildcard_subscription_available": capabilities.wildcard_subscription_available,
            "subscription_identifier_available": capabilities.subscription_identifier_available,
            "shared_subscription_available": capabilities.shared_subscription_available,
            "injected_user_properties": capabilities.injected_user_properties,
        },
        "tests": tests_by_id,
    });
    let serialized = serde_json::to_string_pretty(&report).map_err(std::io::Error::other)?;
    std::fs::write(path, serialized)
}

fn collect_test_results(capabilities: &Capabilities) -> Vec<serde_json::Value> {
    CONFORMANCE_TESTS
        .iter()
        .map(|test| {
            let unmet = first_unmet_requirement(capabilities, test.requires);
            let status = if unmet.is_some() {
                "skipped"
            } else {
                "planned"
            };
            serde_json::json!({
                "name": test.name,
                "module_path": test.module_path,
                "file": test.file,
                "line": test.line,
                "ids": test.ids,
                "status": status,
                "unmet_requirement": unmet.map(Requirement::label),
            })
        })
        .collect()
}
