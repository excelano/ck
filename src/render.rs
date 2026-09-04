//! What the caller reads.
//!
//! Shadow mode only, for now: the verdict as the parse produced it, then the
//! raw output underneath, so a parse that lies is visible in the same
//! screenful as the truth. When a baseline store exists the verdict becomes
//! a comparison; the shape of this view does not change.

use std::io::Write;

use crate::adapter;
use crate::report::{Confidence, Parse, RunReport, Severity, Tier};

/// Print the shadow-mode view and return the exit code.
pub fn verify(report: &RunReport) -> i32 {
    let code = crate::exec::exit_code(&report.raw.status);
    let mut out = std::io::stdout().lock();
    let _ = write_verdict(&mut out, report, code);
    let _ = writeln!(out, "---- raw output ----");
    let _ = out.flush();
    drop(out);
    adapter::dump_raw(report.adapter, &report.raw);
    code
}

fn write_verdict(out: &mut impl Write, report: &RunReport, code: i32) -> std::io::Result<()> {
    let errors = report
        .failures
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    let warnings = report.failures.len() - errors;
    write!(out, "ck --verify:")?;
    // The runner's own counts come first when it gave them, because those
    // are what the caller can check against expectation. The counts of what
    // ck parsed follow, and they are ck's claim rather than the runner's.
    if let Some(t) = report.totals {
        write!(
            out,
            " {} passed, {} failed, {} ignored;",
            t.passed, t.failed, t.ignored
        )?;
    }
    writeln!(
        out,
        " {} {}, {} {}, exit {code}",
        errors,
        plural(errors, "error"),
        warnings,
        plural(warnings, "warning"),
    )?;

    for f in &report.failures {
        let rule = f.rule.as_deref().unwrap_or("-");
        let severity = match f.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        match &f.location {
            Some(l) => writeln!(
                out,
                "  {severity}[{rule}] {}:{}:{}  {}",
                l.file, l.line, l.column, f.message
            )?,
            None => writeln!(out, "  {severity}[{rule}]  {}", f.message)?,
        }
        let mut marks = Vec::new();
        if f.identity.tier != Tier::Node {
            marks.push(match f.identity.tier {
                Tier::Constructed => "constructed",
                Tier::Hash => "hash",
                Tier::Node => unreachable!(),
            });
        }
        if f.identity.confidence == Confidence::Low {
            marks.push("low confidence");
        }
        writeln!(
            out,
            "      id {}  {}  [{}]",
            f.identity.handle(),
            f.identity.key,
            marks.join(", ")
        )?;
    }

    match &report.parse {
        Parse::Clean => {}
        Parse::Partial => writeln!(out, "  output after the build was not parsed")?,
        Parse::Failed(why) => writeln!(out, "  parse failed: {why}; the raw output is the answer")?,
    }
    // Principle 3: a clean parse that found nothing while the command failed
    // is itself the finding.
    if report.failures.is_empty() && code != 0 {
        writeln!(
            out,
            "  nothing found, but the command exited {code}: not a clean run"
        )?;
    }
    Ok(())
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}
