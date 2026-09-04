//! Compiler diagnostics out of cargo's JSON stream, into the normalized
//! record.
//!
//! One `compiler-message` line per diagnostic, up to `build-finished`. Each
//! becomes a failure with a constructed identity: the primary span's file,
//! the code, and a discriminator lifted from the message — the backticked
//! names, which is what distinguishes one `E0425` from another. Line
//! numbers are payload, never identity, because they move with every edit
//! above them.
//!
//! The crude key has a known collision, found on the first scenario: one
//! renamed function produces one identical diagnostic per call site, all in
//! one file with the same discriminator. Identical keys are numbered in
//! source order, so three call sites are three failures. Fixing the first
//! renumbers the rest, which reports as churn; that is the loud direction,
//! and the one principle 1 accepts.
//!
//! Cargo emits the same diagnostic once per compile unit that hits it — the
//! lib and the lib's tests, typically — so identical diagnostics collapse
//! to one before anything is counted.

use serde_json::Value;

use crate::adapter::AdapterId;
use crate::exec::{Captured, Stream};
use crate::report::{Confidence, Failure, Identity, Location, Parse, RunReport, Severity, Tier};

/// Parse a captured cargo run.
pub fn parse(captured: Captured) -> RunReport {
    let (diagnostics, parse) = read(&captured);
    RunReport {
        adapter: AdapterId::Cargo,
        parse,
        totals: None,
        failures: into_failures(diagnostics),
        raw: captured,
    }
}

/// One diagnostic as rustc described it, before identity is assigned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Diagnostic {
    severity: Severity,
    code: Option<String>,
    location: Option<Location>,
    message: String,
    rendered: Option<String>,
}

/// Walk stdout. Everything before `build-finished` must be cargo's JSON or
/// the parse is not trusted; everything after it is somebody else's.
fn read(captured: &Captured) -> (Vec<Diagnostic>, Parse) {
    let stdout: Vec<u8> = captured
        .chunks
        .iter()
        .filter(|c| c.stream == Stream::Stdout)
        .flat_map(|c| c.bytes.iter().copied())
        .collect();

    let mut diagnostics = Vec::new();
    let mut finished = false;
    let mut trailing = false;
    for line in stdout.split_inclusive(|&b| b == b'\n') {
        if finished {
            trailing = true;
            break;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return (
                diagnostics,
                Parse::Failed("unexpected text before the build finished".into()),
            );
        };
        match value.get("reason").and_then(Value::as_str) {
            Some("compiler-message") => {
                if let Some(d) = diagnostic(&value) {
                    diagnostics.push(d);
                }
            }
            Some("build-finished") => finished = true,
            Some(_) => {}
            None => {
                return (
                    diagnostics,
                    Parse::Failed("a line without a reason before the build finished".into()),
                );
            }
        }
    }

    let parse = if !finished {
        Parse::Failed("the build never reported finishing".into())
    } else if trailing {
        Parse::Partial
    } else {
        Parse::Clean
    };
    (diagnostics, parse)
}

/// Lift one `compiler-message` into a diagnostic, or `None` for the levels
/// that are commentary rather than findings.
fn diagnostic(value: &Value) -> Option<Diagnostic> {
    let message = value.get("message")?;
    let severity = match message.get("level").and_then(Value::as_str)? {
        "error" | "error: internal compiler error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => return None,
    };
    let code = message
        .pointer("/code/code")
        .and_then(Value::as_str)
        .map(str::to_string);
    let location = message
        .get("spans")
        .and_then(Value::as_array)
        .and_then(|spans| {
            spans
                .iter()
                .find(|s| s.get("is_primary") == Some(&Value::Bool(true)))
        })
        .and_then(|span| {
            Some(Location {
                file: span.get("file_name")?.as_str()?.to_string(),
                line: span.get("line_start")?.as_u64()? as usize,
                column: span.get("column_start")?.as_u64()? as usize,
            })
        });
    Some(Diagnostic {
        severity,
        code,
        location,
        message: message.get("message")?.as_str()?.to_string(),
        rendered: message
            .get("rendered")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Deduplicate, order, and assign identities.
fn into_failures(mut diagnostics: Vec<Diagnostic>) -> Vec<Failure> {
    let mut seen: Vec<Diagnostic> = Vec::new();
    diagnostics.retain(|d| {
        let duplicate = seen
            .iter()
            .any(|s| s.location == d.location && s.code == d.code && s.message == d.message);
        if !duplicate {
            seen.push(d.clone());
        }
        !duplicate
    });
    diagnostics.sort_by(|a, b| {
        let key = |d: &Diagnostic| {
            d.location
                .as_ref()
                .map(|l| (l.file.clone(), l.line, l.column))
        };
        key(a).cmp(&key(b))
    });

    let mut failures: Vec<Failure> = Vec::with_capacity(diagnostics.len());
    for d in diagnostics {
        let identity = identity(&d, &failures);
        failures.push(Failure {
            identity,
            severity: d.severity,
            rule: d.code,
            location: d.location,
            message: d.message,
            detail: d.rendered,
        });
    }
    failures
}

/// Assign an identity, numbering it past any earlier failure with the same
/// base key.
fn identity(d: &Diagnostic, earlier: &[Failure]) -> Identity {
    let Some(location) = &d.location else {
        // Nothing to anchor it to. The message itself is the key.
        return Identity {
            tier: Tier::Hash,
            confidence: Confidence::Low,
            key: format!(
                "{}|{}",
                d.code.as_deref().unwrap_or("-"),
                normalize(&d.message)
            ),
        };
    };

    let (discriminator, mut confidence) = match backticked(&d.message) {
        Some(names) => (names, Confidence::Canonical),
        None => (normalize(&d.message), Confidence::Low),
    };
    if d.code.is_none() {
        confidence = Confidence::Low;
    }
    let base = format!(
        "{}|{}|{}",
        location.file,
        d.code.as_deref().unwrap_or("-"),
        discriminator
    );

    let same = earlier
        .iter()
        .filter(|f| f.identity.tier == Tier::Constructed && base_of(&f.identity.key) == base)
        .count();
    let key = if same == 0 {
        base
    } else {
        confidence = Confidence::Low;
        format!("{base}#{}", same + 1)
    };
    Identity {
        tier: Tier::Constructed,
        confidence,
        key,
    }
}

/// The key without its occurrence suffix.
fn base_of(key: &str) -> &str {
    match key.rsplit_once('#') {
        Some((base, n)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => base,
        _ => key,
    }
}

/// The backticked names in a message, joined. `None` when there are none.
fn backticked(message: &str) -> Option<String> {
    let names: Vec<&str> = message
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        None
    } else {
        Some(names.join(","))
    }
}

/// A message with its numbers templated out, so a count that changes does
/// not change the key.
fn normalize(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut in_number = false;
    for c in message.chars() {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('#');
                in_number = true;
            }
        } else {
            in_number = false;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Chunk;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn captured(stdout: &str, code: i32) -> Captured {
        Captured {
            chunks: vec![Chunk {
                stream: Stream::Stdout,
                bytes: stdout.as_bytes().to_vec(),
            }],
            status: ExitStatus::from_raw(code << 8),
        }
    }

    macro_rules! fixture {
        ($name:literal, $code:expr) => {
            parse(captured(
                include_str!(concat!("../../../tests/fixtures/cargo/", $name, ".jsonl")),
                $code,
            ))
        };
    }

    fn keys(report: &RunReport) -> Vec<&str> {
        report
            .failures
            .iter()
            .map(|f| f.identity.key.as_str())
            .collect()
    }

    #[test]
    fn a_green_build_is_clean_and_empty() {
        let report = fixture!("green-build", 0);
        assert_eq!(report.parse, Parse::Clean);
        assert!(report.failures.is_empty());
        assert_eq!(report.totals, None);
    }

    #[test]
    fn a_green_test_run_is_partial_because_the_tests_follow_the_build() {
        let report = fixture!("green-test", 0);
        assert_eq!(report.parse, Parse::Partial);
        assert!(report.failures.is_empty());
    }

    #[test]
    fn one_error_at_one_site() {
        let report = fixture!("rename-build", 101);
        assert_eq!(report.parse, Parse::Clean);
        assert_eq!(keys(&report), ["src/stats.rs|E0425|frequencies"]);
        let f = &report.failures[0];
        assert_eq!(f.severity, Severity::Error);
        assert_eq!(f.rule.as_deref(), Some("E0425"));
        assert_eq!(
            f.location,
            Some(Location {
                file: "src/stats.rs".into(),
                line: 17,
                column: 43
            })
        );
        assert_eq!(f.identity.tier, Tier::Constructed);
        assert_eq!(f.identity.confidence, Confidence::Canonical);
        assert!(f.detail.as_deref().unwrap().starts_with("error[E0425]"));
    }

    #[test]
    fn the_same_error_at_two_sites_is_two_failures_not_one_and_not_three() {
        // Cargo reports the line-17 error for both the lib and its tests, and
        // the line-41 error (inside a test) once. Two sites, two failures.
        let report = fixture!("rename-test", 101);
        assert_eq!(report.parse, Parse::Clean);
        assert_eq!(
            keys(&report),
            [
                "src/stats.rs|E0425|frequencies",
                "src/stats.rs|E0425|frequencies#2"
            ]
        );
        assert_eq!(
            report.failures[0].identity.confidence,
            Confidence::Canonical
        );
        assert_eq!(report.failures[1].identity.confidence, Confidence::Low);
        assert_eq!(report.failures[1].location.as_ref().unwrap().line, 41);
    }

    #[test]
    fn different_codes_are_different_failures_even_on_adjacent_lines() {
        let report = fixture!("signature-test", 101);
        assert_eq!(report.parse, Parse::Clean);
        let rules: Vec<&str> = report
            .failures
            .iter()
            .map(|f| f.rule.as_deref().unwrap())
            .collect();
        assert_eq!(rules, ["E0631", "E0599"]);
        // No backticks in the E0631 message, so the discriminator is the
        // message itself and the key is marked accordingly.
        assert_eq!(report.failures[0].identity.confidence, Confidence::Low);
        assert_eq!(
            report.failures[1].identity.confidence,
            Confidence::Canonical
        );
    }

    #[test]
    fn warnings_are_failures_with_their_lint_as_the_rule() {
        let report = fixture!("warnings-check", 0);
        assert_eq!(
            keys(&report),
            [
                "src/stats.rs|unused_imports|std::fmt::Debug",
                "src/tokens.rs|unused_variables|unused"
            ]
        );
        assert!(
            report
                .failures
                .iter()
                .all(|f| f.severity == Severity::Warning)
        );
    }

    #[test]
    fn clippy_lints_carry_their_full_name() {
        let report = fixture!("clippy-lints", 0);
        let rules: Vec<&str> = report
            .failures
            .iter()
            .map(|f| f.rule.as_deref().unwrap())
            .collect();
        assert_eq!(rules, ["clippy::bool_comparison", "clippy::len_zero"]);
    }

    #[test]
    fn failing_tests_are_not_diagnostics() {
        // The build was clean; what failed came after it and is not parsed
        // here. That is Partial, and the exit code disagreement is the
        // renderer's to report.
        let report = fixture!("failing-test", 101);
        assert_eq!(report.parse, Parse::Partial);
        assert!(report.failures.is_empty());
    }

    #[test]
    fn text_before_the_build_finishes_is_not_trusted() {
        let report = parse(captured("not json at all\n", 1));
        assert!(matches!(report.parse, Parse::Failed(_)));
    }

    #[test]
    fn a_build_that_never_finishes_is_not_trusted() {
        let report = parse(captured(
            r#"{"reason":"compiler-artifact","target":{}}"#,
            137,
        ));
        assert!(matches!(report.parse, Parse::Failed(_)));
    }

    #[test]
    fn a_diagnostic_without_a_span_falls_to_the_hash_tier() {
        let line = r#"{"reason":"compiler-message","message":{"level":"error","code":null,"spans":[],"message":"linking with `cc` failed: exit status: 1","rendered":"error: linking with `cc` failed\n"}}
{"reason":"build-finished","success":false}
"#;
        let report = parse(captured(line, 101));
        let f = &report.failures[0];
        assert_eq!(f.identity.tier, Tier::Hash);
        assert_eq!(f.identity.key, "-|linking with `cc` failed: exit status: #");
        assert_eq!(f.location, None);
    }

    #[test]
    fn backticked_names_are_the_discriminator() {
        assert_eq!(
            backticked("cannot find `a` and `b`").as_deref(),
            Some("a,b")
        );
        assert_eq!(backticked("no names here"), None);
    }

    #[test]
    fn numbers_are_templated_out_of_messages() {
        assert_eq!(
            normalize("expected 3 arguments, found 12"),
            "expected # arguments, found #"
        );
    }
}
