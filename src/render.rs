//! What the caller reads.
//!
//! One shape for every compared run: a summary line that carries absolute
//! counts, then the sections that have anything in them. NEW gets full
//! detail, because it is what the caller acts on. STILL FAILING gets one
//! line each, because the caller already knows about those. FIXED gets a
//! count. The escape hatch to suppressed detail is printed at the bottom
//! rather than documented in `--help`, so the moment a caller needs it the
//! answer is on the screen.
//!
//! Every line ck writes on its own behalf begins with `ck:` or sits in a
//! section of its own, so nothing here can be mistaken for the runner.

use std::io::Write;

use crate::compare::Comparison;
use crate::report::{Confidence, Failure, Parse, RunReport, Severity, Tier, Totals};
use crate::store::Stored;

/// The one line printed after a first-contact run, underneath the raw
/// output. The counts are what the caller can check against what they
/// just read.
pub fn first_contact(out: &mut impl Write, report: &RunReport) -> std::io::Result<()> {
    let (errors, warnings) = count(report.failures.iter());
    writeln!(
        out,
        "ck: baseline recorded ({}); the next run reports what changed",
        errors_and_warnings(errors, warnings)
    )
}

/// The verdict on a compared run.
pub fn comparison(
    out: &mut impl Write,
    c: &Comparison,
    totals: Option<Totals>,
) -> std::io::Result<()> {
    let current = c.new.iter().chain(c.still.iter().map(|s| &s.failure));
    let (errors, warnings) = count(current);

    write!(out, "ck: ")?;
    if let Some(t) = totals {
        write!(
            out,
            "{} passed, {} failed, {} ignored; ",
            t.passed, t.failed, t.ignored
        )?;
    }
    write!(out, "{}", errors_and_warnings(errors, warnings))?;
    if c.is_quiet() {
        writeln!(out, ", nothing new")?;
    } else {
        writeln!(out, ", {} new, {} fixed", c.new.len(), c.fixed.len())?;
    }

    if !c.new.is_empty() {
        writeln!(out, "\nNEW ({})", c.new.len())?;
        for f in &c.new {
            match &f.detail {
                Some(detail) => {
                    for line in detail.trim_end().lines() {
                        if line.is_empty() {
                            writeln!(out)?;
                        } else {
                            writeln!(out, "  {line}")?;
                        }
                    }
                }
                None => writeln!(out, "  {}", headline(f))?,
            }
            if let Some(mark) = weak(f) {
                writeln!(out, "  [{mark}]")?;
            }
        }
    }

    if !c.still.is_empty() {
        writeln!(out, "\nSTILL FAILING ({})", c.still.len())?;
        for s in &c.still {
            let runs = if s.runs >= Stored::HISTORY {
                format!("{}+ runs", Stored::HISTORY)
            } else {
                format!("{} runs", s.runs)
            };
            write!(
                out,
                "  {}  {}  ({runs}",
                s.failure.identity.handle(),
                headline(&s.failure)
            )?;
            if let Some(mark) = weak(&s.failure) {
                write!(out, ", {mark}")?;
            }
            writeln!(out, ")")?;
        }
        writeln!(out, "\n  detail: ck show <id>")?;
    }

    if !c.fixed.is_empty() {
        writeln!(out, "\nFIXED ({})", c.fixed.len())?;
    }
    Ok(())
}

/// Shadow mode's middle section: what the parse produced, with identities
/// spelled out, so a verdict that lies is visible next to the raw output
/// that would expose it.
pub fn parsed(out: &mut impl Write, report: &RunReport, code: i32) -> std::io::Result<()> {
    let (errors, warnings) = count(report.failures.iter());
    writeln!(out, "---- parsed ----")?;
    write!(out, "{}", errors_and_warnings(errors, warnings))?;
    if let Some(t) = report.totals {
        write!(
            out,
            "; {} passed, {} failed, {} ignored",
            t.passed, t.failed, t.ignored
        )?;
    }
    writeln!(out, ", exit {code}")?;

    for f in &report.failures {
        writeln!(out, "  {}", headline(f))?;
        let mut marks = Vec::new();
        match f.identity.tier {
            Tier::Node => {}
            Tier::Constructed => marks.push("constructed"),
            Tier::Hash => marks.push("hash"),
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
    if crate::compare::unaccounted(report, code) {
        writeln!(
            out,
            "  no error found, but the command exited {code}: not a clean run"
        )?;
    }
    Ok(())
}

pub const RAW_SEPARATOR: &str = "---- raw output ----";

/// `error[E0425] src/lib.rs:4:9  message`, the one-line form.
pub fn headline(f: &Failure) -> String {
    let rule = f.rule.as_deref().unwrap_or("-");
    let severity = match f.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    match &f.location {
        Some(l) => format!(
            "{severity}[{rule}] {}:{}:{}  {}",
            l.file, l.line, l.column, f.message
        ),
        None => format!("{severity}[{rule}]  {}", f.message),
    }
}

/// The marker for an identity the caller should not trust all the way.
/// The trustworthy tier gets nothing: billing the caller for a fact that
/// is true by default is noise.
fn weak(f: &Failure) -> Option<String> {
    let hash = f.identity.tier == Tier::Hash;
    let low = f.identity.confidence == Confidence::Low;
    match (hash, low) {
        (true, true) => Some("hash identity, low confidence".into()),
        (true, false) => Some("hash identity".into()),
        (false, true) => Some("low confidence".into()),
        (false, false) => None,
    }
}

fn count<'a>(failures: impl Iterator<Item = &'a Failure>) -> (usize, usize) {
    failures.fold((0, 0), |(e, w), f| match f.severity {
        Severity::Error => (e + 1, w),
        Severity::Warning => (e, w + 1),
    })
}

fn errors_and_warnings(errors: usize, warnings: usize) -> String {
    format!(
        "{errors} {}, {warnings} {}",
        plural(errors, "error"),
        plural(warnings, "warning")
    )
}

pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}
