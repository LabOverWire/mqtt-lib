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
//! mqtt5-conformance --client mqtt5               # in-process mqtt5 client
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
    client_sut::ClientSutHandle,
    registry::{ConformanceTest, TestKind, CONFORMANCE_TESTS},
    sut::{SutDescriptor, SutHandle},
};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};
use tokio::runtime::Runtime;

fn main() -> ExitCode {
    if cfg!(panic = "abort") {
        eprintln!(
            "error: this binary was built with panic = \"abort\" and would stop at the first failing test; build it with `cargo build --profile conformance -p mqtt5-conformance-cli`"
        );
        return ExitCode::from(2);
    }

    let (options, libtest_args) = CliOptions::from_env();

    let runtime = match Runtime::new() {
        Ok(runtime) => Arc::new(runtime),
        Err(error) => {
            eprintln!("error: failed to start tokio runtime: {error}");
            return ExitCode::from(2);
        }
    };

    let mode = match Mode::from_options(&options, &runtime) {
        Ok(mode) => Arc::new(mode),
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };

    let outcomes: Outcomes = CONFORMANCE_TESTS.iter().map(|_| OnceLock::new()).collect();
    let trials = build_trials(&mode, &runtime, &outcomes);
    let conclusion = libtest_mimic::run(&libtest_args, trials);

    if let Some(report_path) = options.report.as_deref() {
        if let Err(error) = write_report(report_path, &mode, &conclusion, &outcomes) {
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
    client: Option<String>,
    report: Option<PathBuf>,
}

impl CliOptions {
    fn from_env() -> (Self, Arguments) {
        let mut sut: Option<PathBuf> = None;
        let mut client: Option<String> = None;
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
            } else if let Some(value) = arg.strip_prefix("--client=") {
                client = Some(value.to_owned());
            } else if arg == "--client" {
                client = raw.next();
            } else if let Some(value) = arg.strip_prefix("--report=") {
                report = Some(PathBuf::from(value));
            } else if arg == "--report" {
                report = raw.next().map(PathBuf::from);
            } else {
                forwarded.push(arg);
            }
        }

        let libtest = Arguments::from_iter(forwarded);
        (
            Self {
                sut,
                client,
                report,
            },
            libtest,
        )
    }
}

type Outcomes = Arc<[OnceLock<Result<(), String>>]>;

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

enum Mode {
    Server {
        plan: SutPlan,
        capabilities: Capabilities,
    },
    Client(ClientSutHandle),
}

impl Mode {
    fn from_options(options: &CliOptions, runtime: &Runtime) -> Result<Self, String> {
        match (&options.client, &options.sut) {
            (Some(_), Some(_)) => Err("--client and --sut cannot be combined".to_owned()),
            (Some(name), None) if name == "mqtt5" => Ok(Self::Client(
                mqtt5_conformance::client_sut::inprocess_client(),
            )),
            (Some(name), None) => Err(format!(
                "unknown client {name:?} (the only built-in client is \"mqtt5\")"
            )),
            (None, _) => {
                let plan = SutPlan::from_options(options)?;
                let capabilities = plan.capabilities_snapshot(runtime);
                Ok(Self::Server { plan, capabilities })
            }
        }
    }

    fn selects(&self, test: &ConformanceTest) -> bool {
        matches!(
            (self, test.kind),
            (Self::Server { .. }, TestKind::Server { .. })
                | (Self::Client(_), TestKind::Client { .. })
        )
    }

    fn unmet_requirement(&self, test: &ConformanceTest) -> Option<String> {
        match (self, test.kind) {
            (Self::Server { capabilities, .. }, TestKind::Server { requires, .. }) => {
                first_unmet_requirement(capabilities, requires).map(Requirement::label)
            }
            (Self::Client(sut), TestKind::Client { requires, .. }) => {
                let capabilities = sut.capabilities();
                requires
                    .iter()
                    .copied()
                    .find(|requirement| !requirement.matches(&capabilities))
                    .map(|requirement| requirement.label().to_owned())
            }
            _ => None,
        }
    }
}

fn build_trials(mode: &Arc<Mode>, runtime: &Arc<Runtime>, outcomes: &Outcomes) -> Vec<Trial> {
    let mut trials = Vec::with_capacity(CONFORMANCE_TESTS.len());
    for (index, test) in CONFORMANCE_TESTS.iter().copied().enumerate() {
        if !mode.selects(&test) {
            continue;
        }
        let name = trial_name(&test);
        let trial = if let Some(label) = mode.unmet_requirement(&test) {
            Trial::test(format!("{name} [missing: {label}]"), || Ok(())).with_ignored_flag(true)
        } else {
            let mode = Arc::clone(mode);
            let runtime = Arc::clone(runtime);
            let outcomes = Arc::clone(outcomes);
            Trial::test(name, move || {
                let outcome = run_trial(&mode, &runtime, &test);
                outcomes[index].get_or_init(|| outcome.clone());
                outcome.map_err(Failed::from)
            })
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

fn run_trial(mode: &Mode, runtime: &Runtime, test: &ConformanceTest) -> Result<(), String> {
    let future = match (mode, test.kind) {
        (Mode::Server { plan, .. }, TestKind::Server { runner, .. }) => {
            std::panic::catch_unwind(AssertUnwindSafe(|| plan.build_handle(runtime))).map(runner)
        }
        (Mode::Client(sut), TestKind::Client { runner, .. }) => Ok(runner(Arc::clone(sut))),
        _ => return Err(format!("{}: not a test for this mode", test.name)),
    };
    future
        .and_then(|future| {
            runtime.block_on(async {
                let result = AssertUnwindSafe(future).catch_unwind().await;
                tokio::task::yield_now().await;
                result
            })
        })
        .map_err(|payload| format!("{}: {}", test.name, panic_message(payload.as_ref())))
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
    mode: &Mode,
    conclusion: &Conclusion,
    outcomes: &Outcomes,
) -> std::io::Result<()> {
    let tests_by_id = collect_test_results(mode, outcomes);
    let capabilities = match mode {
        Mode::Server { capabilities, .. } => serde_json::json!({
            "max_qos": capabilities.max_qos,
            "retain_available": capabilities.retain_available,
            "wildcard_subscription_available": capabilities.wildcard_subscription_available,
            "subscription_identifier_available": capabilities.subscription_identifier_available,
            "shared_subscription_available": capabilities.shared_subscription_available,
            "injected_user_properties": capabilities.injected_user_properties,
        }),
        Mode::Client(sut) => {
            let capabilities = sut.capabilities();
            serde_json::json!({
                "client": sut.name(),
                "automatic_reconnect": capabilities.automatic_reconnect,
                "offline_queue": capabilities.offline_queue,
                "enhanced_auth": capabilities.enhanced_auth,
            })
        }
    };
    let report = serde_json::json!({
        "summary": {
            "passed": conclusion.num_passed,
            "failed": conclusion.num_failed,
            "ignored": conclusion.num_ignored,
            "filtered_out": conclusion.num_filtered_out,
            "measured": conclusion.num_measured,
        },
        "capabilities": capabilities,
        "tests": tests_by_id,
    });
    let serialized = serde_json::to_string_pretty(&report).map_err(std::io::Error::other)?;
    std::fs::write(path, serialized)
}

fn collect_test_results(mode: &Mode, outcomes: &Outcomes) -> Vec<serde_json::Value> {
    CONFORMANCE_TESTS
        .iter()
        .zip(outcomes.iter())
        .filter(|(test, _)| mode.selects(test))
        .map(|(test, outcome)| {
            let unmet = mode.unmet_requirement(test);
            let (status, failure) = match (&unmet, outcome.get()) {
                (Some(_), _) => ("skipped", None),
                (None, Some(Ok(()))) => ("passed", None),
                (None, Some(Err(message))) => ("failed", Some(message)),
                (None, None) => ("not_run", None),
            };
            serde_json::json!({
                "name": test.name,
                "module_path": test.module_path,
                "file": test.file,
                "line": test.line,
                "ids": test.ids,
                "status": status,
                "failure": failure,
                "unmet_requirement": unmet,
            })
        })
        .collect()
}
