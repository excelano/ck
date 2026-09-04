//! What changed since the last run.
//!
//! Pure: a previous baseline and a fresh report go in, the verdict and the
//! next baseline come out, and nothing here touches the disk or the
//! terminal. The semantics are last-run semantics throughout. The baseline
//! advances on every completed run, so "new" means "was not failing on the
//! previous run", which includes a failure that had been fixed and has come
//! back.
//!
//! Matching is on the identity key and nothing else. Location is payload,
//! message is payload, and a failure that moved twelve lines down is the
//! same failure; that is the whole reason identity is computed at ingest.

use crate::report::{Failure, RunReport, Severity};
use crate::store::{Baseline, CommandLine, SCHEMA, Stored};

/// A failure that was there last run and is there again.
#[derive(Debug, PartialEq, Eq)]
pub struct Streak {
    pub failure: Failure,
    /// Consecutive runs it has been failing, this one included. What the
    /// caller cannot reconstruct once detail stops printing. Saturates at
    /// `Stored::HISTORY`, since that is as far back as anything remembers.
    pub runs: usize,
}

/// The verdict on one run, and the baseline it becomes.
#[derive(Debug)]
pub struct Comparison {
    /// Not failing on the previous run. Gets full detail.
    pub new: Vec<Failure>,
    /// Failing on the previous run too. Gets identity only.
    pub still: Vec<Streak>,
    /// Failing on the previous run and not on this one.
    pub fixed: Vec<Failure>,
    pub next: Baseline,
}

impl Comparison {
    /// Whether the run has anything to report beyond a count.
    pub fn is_quiet(&self) -> bool {
        self.new.is_empty() && self.fixed.is_empty()
    }
}

/// Passing runs after which a failure is forgotten. Long enough for a
/// flake to be recognised as one, short enough that the store does not
/// remember every failure a project ever had.
const FORGET_AFTER: usize = 8;

/// Compare `report` against what `previous` recorded. `None` is first
/// contact: everything is new and nothing is fixed, and the caller
/// decides how much of that to say.
pub fn compare(
    previous: Option<Baseline>,
    report: &RunReport,
    command: CommandLine,
    exit: i32,
) -> Comparison {
    let mut new = Vec::new();
    let mut still = Vec::new();
    let mut fixed = Vec::new();
    let mut failures = Vec::with_capacity(report.failures.len());

    let (runs, mut remembered) = match previous {
        Some(b) => (b.runs + 1, b.failures),
        None => (1, Vec::new()),
    };

    for failure in &report.failures {
        let position = remembered
            .iter()
            .position(|s| s.failure.identity.key == failure.identity.key);
        let history = match position {
            Some(i) => {
                let stored = remembered.swap_remove(i);
                // Last-run semantics: what it did the run before decides
                // whether it is new now, however long ago it first appeared.
                if stored.history.ends_with('F') {
                    still.push(Streak {
                        failure: failure.clone(),
                        runs: (trailing(&stored.history, 'F') + 1).min(Stored::HISTORY),
                    });
                } else {
                    new.push(failure.clone());
                }
                extend(stored.history, 'F')
            }
            None => {
                new.push(failure.clone());
                "F".to_string()
            }
        };
        // The record is the fresh one: the location may have moved, the
        // detail is current, and the identity is the same by construction.
        failures.push(Stored {
            failure: failure.clone(),
            history,
        });
    }

    // Whatever is left in `remembered` did not happen this run.
    for stored in remembered {
        if stored.history.ends_with('F') {
            fixed.push(stored.failure.clone());
        }
        let history = extend(stored.history, 'P');
        if trailing(&history, 'P') < FORGET_AFTER {
            failures.push(Stored {
                failure: stored.failure,
                history,
            });
        }
    }

    // Forgotten failures came off the end in baseline order; everything
    // seen this run is in the report's order, which is the file's.
    let next = Baseline {
        schema: SCHEMA,
        ck: env!("CARGO_PKG_VERSION").to_string(),
        command,
        adapter: report.adapter,
        recorded_at: crate::store::now(),
        runs,
        exit,
        totals: report.totals,
        failures,
    };

    Comparison {
        new,
        still,
        fixed,
        next,
    }
}

/// The exit code for a compared run. The gate is the reason the tool
/// exists, so its rule is stated once, here: `1` when something new is
/// failing and the runner agrees that it is failing, `0` otherwise. A new
/// warning never gates, because the runner would not have failed on it and
/// ck is never stricter than the runner about what counts as a failure.
pub fn gate(comparison: &Comparison, child: i32) -> i32 {
    let new_error = comparison.new.iter().any(|f| f.severity == Severity::Error);
    if child != 0 && new_error { 1 } else { 0 }
}

/// Whether the child failed for a reason the parse did not find. A clean
/// parse that saw no errors while the command exited non-zero cannot be
/// compared: whatever failed is not in the report, so "nothing new" would
/// be a lie. The raw output is the answer in that case.
pub fn unaccounted(report: &RunReport, child: i32) -> bool {
    child != 0
        && !report
            .failures
            .iter()
            .any(|f| f.severity == Severity::Error)
}

fn extend(mut history: String, outcome: char) -> String {
    history.push(outcome);
    if history.len() > Stored::HISTORY {
        history.drain(..history.len() - Stored::HISTORY);
    }
    history
}

fn trailing(history: &str, outcome: char) -> usize {
    history.chars().rev().take_while(|&c| c == outcome).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterId;
    use crate::exec::Captured;
    use crate::report::{Confidence, Identity, Parse, Tier};
    use std::os::unix::process::ExitStatusExt;

    fn failure(key: &str, severity: Severity) -> Failure {
        Failure {
            identity: Identity {
                tier: Tier::Constructed,
                confidence: Confidence::Canonical,
                key: key.into(),
            },
            severity,
            rule: None,
            location: None,
            message: format!("message for {key}"),
            detail: None,
        }
    }

    fn error(key: &str) -> Failure {
        failure(key, Severity::Error)
    }

    fn report(failures: Vec<Failure>, code: i32) -> RunReport {
        RunReport {
            adapter: AdapterId::Cargo,
            parse: Parse::Clean,
            totals: None,
            failures,
            raw: Captured {
                chunks: Vec::new(),
                status: std::process::ExitStatus::from_raw(code << 8),
            },
        }
    }

    fn command() -> CommandLine {
        CommandLine {
            env: Vec::new(),
            argv: vec!["cargo".into(), "check".into()],
            dir: String::new(),
        }
    }

    fn run(previous: Option<Baseline>, failures: Vec<Failure>, code: i32) -> Comparison {
        compare(previous, &report(failures, code), command(), code)
    }

    fn keys(failures: &[Failure]) -> Vec<&str> {
        failures.iter().map(|f| f.identity.key.as_str()).collect()
    }

    fn history(baseline: &Baseline, key: &str) -> Option<String> {
        baseline
            .failures
            .iter()
            .find(|s| s.failure.identity.key == key)
            .map(|s| s.history.clone())
    }

    #[test]
    fn first_contact_is_all_new_and_nothing_fixed() {
        let c = run(None, vec![error("a"), error("b")], 101);
        assert_eq!(keys(&c.new), ["a", "b"]);
        assert!(c.still.is_empty());
        assert!(c.fixed.is_empty());
        assert_eq!(c.next.runs, 1);
        assert_eq!(history(&c.next, "a").as_deref(), Some("F"));
    }

    #[test]
    fn the_same_failures_again_are_still_failing_and_quiet() {
        let first = run(None, vec![error("a"), error("b")], 101);
        let c = run(Some(first.next), vec![error("a"), error("b")], 101);
        assert!(c.new.is_empty());
        assert_eq!(c.still.len(), 2);
        assert_eq!(c.still[0].runs, 2);
        assert!(c.fixed.is_empty());
        assert!(c.is_quiet());
        assert_eq!(c.next.runs, 2);
        assert_eq!(history(&c.next, "a").as_deref(), Some("FF"));
    }

    #[test]
    fn one_fixed_one_new_one_still() {
        let first = run(None, vec![error("a"), error("b")], 101);
        let c = run(Some(first.next), vec![error("b"), error("c")], 101);
        assert_eq!(keys(&c.new), ["c"]);
        assert_eq!(c.still.len(), 1);
        assert_eq!(c.still[0].failure.identity.key, "b");
        assert_eq!(keys(&c.fixed), ["a"]);
        assert_eq!(history(&c.next, "a").as_deref(), Some("FP"));
        assert_eq!(history(&c.next, "b").as_deref(), Some("FF"));
        assert_eq!(history(&c.next, "c").as_deref(), Some("F"));
    }

    #[test]
    fn a_failure_that_comes_back_is_new_again() {
        let r1 = run(None, vec![error("a")], 101);
        let r2 = run(Some(r1.next), vec![], 0);
        assert_eq!(keys(&r2.fixed), ["a"]);
        let r3 = run(Some(r2.next), vec![error("a")], 101);
        assert_eq!(keys(&r3.new), ["a"]);
        assert!(r3.still.is_empty());
        assert_eq!(history(&r3.next, "a").as_deref(), Some("FPF"));
    }

    #[test]
    fn the_record_kept_is_the_fresh_one() {
        let first = run(None, vec![error("a")], 101);
        let mut moved = error("a");
        moved.location = Some(crate::report::Location {
            file: "src/lib.rs".into(),
            line: 40,
            column: 1,
        });
        moved.message = "same failure, new line".into();
        let c = run(Some(first.next), vec![moved.clone()], 101);
        assert_eq!(c.still[0].failure, moved);
        assert_eq!(c.next.failures[0].failure, moved);
    }

    #[test]
    fn a_fixed_failure_is_forgotten_after_enough_passing_runs() {
        let mut c = run(None, vec![error("a")], 101);
        for _ in 0..FORGET_AFTER - 1 {
            c = run(Some(c.next), vec![], 0);
            assert!(history(&c.next, "a").is_some());
        }
        c = run(Some(c.next), vec![], 0);
        assert!(history(&c.next, "a").is_none());
        assert!(c.fixed.is_empty());
    }

    #[test]
    fn history_is_capped() {
        let mut c = run(None, vec![error("a")], 101);
        for _ in 0..Stored::HISTORY + 5 {
            c = run(Some(c.next), vec![error("a")], 101);
        }
        let h = history(&c.next, "a").unwrap();
        assert_eq!(h.len(), Stored::HISTORY);
        assert!(h.chars().all(|ch| ch == 'F'));
        assert_eq!(c.still[0].runs, Stored::HISTORY);
    }

    #[test]
    fn the_next_baseline_records_the_run() {
        let c = run(None, vec![error("a")], 101);
        assert_eq!(c.next.schema, SCHEMA);
        assert_eq!(c.next.exit, 101);
        assert_eq!(c.next.adapter, AdapterId::Cargo);
        assert_eq!(c.next.command, command());
        assert!(c.next.recorded_at > 0);
    }

    #[test]
    fn the_gate_opens_only_for_a_new_error_the_runner_failed_on() {
        let c = run(None, vec![error("a")], 101);
        assert_eq!(gate(&c, 101), 1);
        // The runner did not fail: ck is not stricter than the runner.
        assert_eq!(gate(&c, 0), 0);
        let again = run(Some(c.next), vec![error("a")], 101);
        assert_eq!(gate(&again, 101), 0);
    }

    #[test]
    fn a_new_warning_never_gates() {
        let c = run(None, vec![failure("w", Severity::Warning)], 0);
        assert_eq!(gate(&c, 0), 0);
        let c = run(None, vec![failure("w", Severity::Warning), error("e")], 101);
        assert_eq!(gate(&c, 101), 1);
    }

    #[test]
    fn a_failed_child_with_no_errors_found_is_unaccounted_for() {
        assert!(unaccounted(&report(vec![], 101), 101));
        assert!(unaccounted(
            &report(vec![failure("w", Severity::Warning)], 101),
            101
        ));
        assert!(!unaccounted(&report(vec![error("e")], 101), 101));
        assert!(!unaccounted(&report(vec![], 0), 0));
    }
}
