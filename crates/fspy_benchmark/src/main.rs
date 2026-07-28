//! Compares what two fspy builds cost the processes they track.
//!
//! Every earlier attempt at this benchmark compared a pull request run with a
//! baseline recorded by an earlier run on `main`, and kept fighting the same
//! fact: two runs land on two runner instances, and identically configured
//! instances disagree on every absolute and relative measurement by more than
//! a regression worth catching. This harness never compares across runs.
//! Instead the workflow builds the launcher twice — once against the fspy
//! under review and once against the baseline fspy — and this harness
//! interleaves launches of both builds on the same machine, in the same run,
//! as neighboring processes. Whatever the runner instance is doing to the
//! numbers, it is doing to both sides.
//!
//! Each iteration launches every arm — baseline-tracked, head-tracked, and
//! untracked — back to back, cycling every ordering, and yields the ratio of
//! head to baseline per metric. The reported change is the median of those
//! ratios, so it prices exactly one thing: what the fspy change under review
//! costs a tracked process. The untracked arm prices tracking itself, as
//! context.

use std::{
    env,
    ffi::{OsStr, OsString},
    process::{Command, Stdio},
};

const HEAD_LAUNCHER: &str = env!("CARGO_BIN_FILE_FSPY_BENCHMARK_LAUNCHER");
const DYNAMIC_TARGET: &str = env!("CARGO_BIN_FILE_FSPY_BENCHMARK_TARGET");
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const STATIC_TARGET: &str = env!("CARGO_BIN_FILE_FSPY_BENCHMARK_STATIC_TARGET");

/// Threads the target runs, passed through to it. Two, because a process that
/// fspy tracks rarely accesses files from one thread.
const THREADS: &str = "2";

/// What a suite reads out of its launches.
#[derive(Clone, Copy)]
enum Metric {
    /// Wall clock of the launch, from spawn to reap.
    Wall,
    /// The typical batch of opens, as timed inside the target.
    Typical,
}

#[derive(Clone, Copy)]
struct MetricReport {
    name: &'static str,
    metric: Metric,
}

struct Suite {
    /// Opens per target thread, passed through to it.
    opens: &'static str,
    /// Measured iterations. Each one launches every arm once.
    iterations: usize,
    /// Unmeasured iterations run first, to fill caches and settle the runner.
    warmup: usize,
    /// Measurements reported from the launches in this suite.
    reports: &'static [MetricReport],
}

const LAUNCH_REPORTS: &[MetricReport] = &[MetricReport { name: "launch", metric: Metric::Wall }];

/// Opens nothing, so the whole launch is the cost of starting a tracked
/// process. Launches cost several times more wall clock on Windows, so it
/// affords fewer of them in the same time.
const LAUNCH_SUITE: Suite = Suite {
    opens: "0",
    iterations: if cfg!(windows) { 150 } else { 300 },
    warmup: 5,
    reports: LAUNCH_REPORTS,
};

const ACCESS_REPORTS: &[MetricReport] = &[
    MetricReport { name: "access", metric: Metric::Typical },
    MetricReport { name: "access-wall", metric: Metric::Wall },
];

/// Opens timed from inside the target, so they price interception rather than
/// the launch around it. The launcher wall clock from the same runs is also
/// reported to show the end-to-end cost of the workload.
const ACCESS_SUITE: Suite =
    Suite { opens: "2048", iterations: 102, warmup: 3, reports: ACCESS_REPORTS };

struct Backend {
    name: &'static str,
    target: &'static str,
}

fn main() {
    let base_launcher = parse_base_launcher();

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let backends = [
        Backend { name: "dynamic", target: DYNAMIC_TARGET },
        Backend { name: "static", target: STATIC_TARGET },
    ];
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    let backends = [Backend { name: "dynamic", target: DYNAMIC_TARGET }];

    for backend in &backends {
        validate(HEAD_LAUNCHER.as_ref(), backend.target);
        if let Some(base_launcher) = &base_launcher {
            validate(base_launcher, backend.target);
        }
        for suite in [&LAUNCH_SUITE, &ACCESS_SUITE] {
            run_suite(backend, suite, base_launcher.as_deref());
        }
    }
}

fn parse_base_launcher() -> Option<OsString> {
    let mut args = env::args_os().skip(1);
    let base_launcher = match args.next() {
        None => None,
        Some(flag) if flag == "--base-launcher" => {
            Some(args.next().expect("--base-launcher needs a path"))
        }
        Some(flag) => {
            panic!("unknown argument {}, expected --base-launcher PATH", flag.to_string_lossy())
        }
    };
    assert!(args.next().is_none(), "unexpected extra arguments");
    base_launcher
}

/// Measurements from one target launch, in nanoseconds.
#[derive(Clone, Copy, Default)]
struct Measurement {
    wall: f64,
    typical: f64,
}

impl Measurement {
    const fn value(self, metric: Metric) -> f64 {
        match metric {
            Metric::Wall => self.wall,
            Metric::Typical => self.typical,
        }
    }
}

/// One iteration's measurements, one launch per arm.
#[derive(Clone, Copy, Default)]
struct Iteration {
    base: Measurement,
    head: Measurement,
    untracked: Measurement,
}

/// Every ordering of the arms of an iteration. Cycling through all of them —
/// rather than rotating one fixed cycle, which keeps every arm's neighbors
/// the same forever — evens out both what position an arm runs in and which
/// arm's leftovers it runs after.
const ORDERS_WITHOUT_BASE: &[&[Arm]] =
    &[&[Arm::Head, Arm::Untracked], &[Arm::Untracked, Arm::Head]];
const ORDERS_WITH_BASE: &[&[Arm]] = &[
    &[Arm::Head, Arm::Untracked, Arm::Base],
    &[Arm::Untracked, Arm::Base, Arm::Head],
    &[Arm::Base, Arm::Head, Arm::Untracked],
    &[Arm::Base, Arm::Untracked, Arm::Head],
    &[Arm::Untracked, Arm::Head, Arm::Base],
    &[Arm::Head, Arm::Base, Arm::Untracked],
];

#[derive(Clone, Copy)]
enum Arm {
    Head,
    Untracked,
    Base,
}

/// Runs a suite and reports its row.
fn run_suite(backend: &Backend, suite: &Suite, base_launcher: Option<&OsStr>) {
    let orders = if base_launcher.is_some() { ORDERS_WITH_BASE } else { ORDERS_WITHOUT_BASE };
    // Whole cycles only, so that each ordering runs equally often.
    let iterations = suite.iterations.next_multiple_of(orders.len());
    let mut measured = Vec::with_capacity(iterations);
    for index in 0..suite.warmup + iterations {
        let mut iteration = Iteration::default();
        for &arm in orders[index % orders.len()] {
            match arm {
                Arm::Head => {
                    iteration.head = launch(HEAD_LAUNCHER.as_ref(), None, backend, suite);
                }
                Arm::Untracked => {
                    iteration.untracked =
                        launch(HEAD_LAUNCHER.as_ref(), Some("--untracked"), backend, suite);
                }
                Arm::Base => {
                    iteration.base = launch(base_launcher.unwrap(), None, backend, suite);
                }
            }
        }
        if index >= suite.warmup {
            measured.push(iteration);
        }
    }

    for metric_report in suite.reports {
        report(backend, *metric_report, &measured, base_launcher.is_some());
    }
}

/// Prints the suite's row.
#[expect(clippy::print_stdout, reason = "the report is the benchmark's output")]
fn report(
    backend: &Backend,
    metric_report: MetricReport,
    iterations: &[Iteration],
    has_base: bool,
) {
    let name = [backend.name, "/", metric_report.name].concat();
    let summary = summarize(iterations, metric_report.metric, has_base);

    if let Some(change) = summary.change {
        println!(
            "{name:<26} change {:>+6.2}%  [{:>+6.2}% .. {:>+6.2}%]  overhead {:>+8.2}%",
            change.median * 100.0,
            change.first_quartile * 100.0,
            change.third_quartile * 100.0,
            summary.overhead * 100.0,
        );
    } else {
        println!("{name:<26} overhead {:>+8.2}%", summary.overhead * 100.0);
    }
}

struct Summary {
    change: Option<Change>,
    overhead: f64,
}

struct Change {
    median: f64,
    first_quartile: f64,
    third_quartile: f64,
}

fn summarize(iterations: &[Iteration], metric: Metric, has_base: bool) -> Summary {
    let value = |measurement: Measurement| measurement.value(metric);
    let overheads = sorted(iterations.iter().map(|it| value(it.head) / value(it.untracked) - 1.0));
    let overhead = quantile(&overheads, 1, 2);
    let change = has_base.then(|| {
        let changes = sorted(iterations.iter().map(|it| value(it.head) / value(it.base) - 1.0));
        Change {
            median: quantile(&changes, 1, 2),
            first_quartile: quantile(&changes, 1, 4),
            third_quartile: quantile(&changes, 3, 4),
        }
    });
    Summary { change, overhead }
}

fn sorted(values: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut values: Vec<f64> = values.collect();
    values.sort_unstable_by(f64::total_cmp);
    values
}

fn quantile(sorted: &[f64], numerator: usize, denominator: usize) -> f64 {
    sorted[sorted.len() * numerator / denominator]
}

/// Launches one arm and reads both numbers its launcher prints.
fn launch(launcher: &OsStr, mode: Option<&str>, backend: &Backend, suite: &Suite) -> Measurement {
    let mut command = Command::new(launcher);
    if let Some(mode) = mode {
        command.arg(mode);
    }
    let output = command
        .args([backend.target, THREADS, suite.opens])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .expect("failed to run the benchmark launcher");
    assert!(output.status.success(), "benchmark launcher failed: {}", output.status);
    let stdout = str::from_utf8(&output.stdout).expect("launcher output is not utf-8");
    let mut numbers = stdout
        .split_whitespace()
        .map(|number| number.parse().expect("unreadable launcher measurement"));
    let mut next = || numbers.next().expect("launcher reported too few measurements");
    let [wall, typical] = [next(), next()];
    assert!(numbers.next().is_none(), "launcher reported too many measurements");
    Measurement { wall, typical }
}

fn validate(launcher: &OsStr, target: &str) {
    let status = Command::new(launcher)
        .arg("--validate")
        .arg(target)
        // One thread opening one batch: the count matches the target's
        // OPENS_PER_SAMPLE. Fewer would open nothing, which validation
        // catches by failing to capture the access.
        .args(["1", "8"])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .expect("failed to run the benchmark launcher");
    assert!(status.success(), "launcher validation failed for {}", launcher.to_string_lossy());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_suite_reports_batch_and_wall_time() {
        assert_eq!(ACCESS_SUITE.reports.len(), 2);
        assert!(matches!(ACCESS_SUITE.reports[0].metric, Metric::Typical));
        assert!(matches!(ACCESS_SUITE.reports[1].metric, Metric::Wall));

        let iterations = [Iteration {
            base: Measurement { wall: 100.0, typical: 20.0 },
            head: Measurement { wall: 125.0, typical: 22.0 },
            untracked: Measurement { wall: 50.0, typical: 10.0 },
        }];
        let typical = summarize(&iterations, Metric::Typical, true);
        let wall = summarize(&iterations, Metric::Wall, true);

        assert!((typical.change.unwrap().median - 0.1).abs() < f64::EPSILON);
        assert!((wall.change.unwrap().median - 0.25).abs() < f64::EPSILON);
        assert!((wall.overhead - 1.5).abs() < f64::EPSILON);
    }
}
