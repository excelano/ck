//! Reading the store back: bare `ck` and `ck show`.
//!
//! Both answer for the place the caller is standing in, the tree and the
//! branch, across every command recorded there. Neither takes a lock;
//! a rename is atomic, so a reader sees a whole file either way.

use std::io::Write;

use crate::render::{self, plural};
use crate::store::repo::{Branch, Place};
use crate::store::{self, Baseline, Load, Stored};

/// Bare `ck`: what is recorded for this tree and branch, one line per
/// command, so the caller can see what a comparison would be against.
pub fn status() -> i32 {
    let place = here();
    let mut out = std::io::stdout().lock();
    let (found, unusable) = split(store::list(&place));
    for why in &unusable {
        eprintln!("ck: {why}");
    }
    if found.is_empty() {
        let _ = writeln!(
            out,
            "ck: no baselines {}; a command run through ck records one\n\
             \n    ck <command> [args...]\n    ck --help",
            where_(&place)
        );
        return 0;
    }

    let _ = writeln!(
        out,
        "ck: {} {} {}",
        found.len(),
        plural(found.len(), "baseline"),
        where_(&place)
    );
    let commands: Vec<String> = found.iter().map(command_line).collect();
    let width = commands.iter().map(String::len).max().unwrap_or(0);
    let now = store::now();
    for (b, command) in found.iter().zip(&commands) {
        let (errors, warnings) = counts(b);
        let _ = writeln!(
            out,
            "  {command:width$}   {errors} {}, {warnings} {}   {} {}, {}",
            plural(errors, "error"),
            plural(warnings, "warning"),
            b.runs,
            plural(b.runs as usize, "run"),
            ago(now.saturating_sub(b.recorded_at)),
        );
    }
    0
}

/// `ck show <id>`: the stored detail for one failure, by the handle the
/// verdict printed. A handle names the same failure in every baseline
/// that has it, so each is mentioned.
pub fn show(handle: &str) -> i32 {
    let place = here();
    let (found, unusable) = split(store::list(&place));
    for why in &unusable {
        eprintln!("ck: {why}");
    }
    let hits: Vec<(&Baseline, &Stored)> = found
        .iter()
        .flat_map(|b| b.failures.iter().map(move |s| (b, s)))
        .filter(|(_, s)| s.failure.identity.handle() == handle)
        .collect();
    let Some((_, first)) = hits.first() else {
        eprintln!(
            "ck: no failure with id {handle} {}\n\
             \n\
             Handles are printed by a compared run; bare ck lists the baselines here.",
            where_(&place)
        );
        return 2;
    };

    let mut out = std::io::stdout().lock();
    match &first.failure.detail {
        Some(detail) => {
            let _ = writeln!(out, "{}", detail.trim_end());
        }
        None => {
            let _ = writeln!(out, "{}", render::headline(&first.failure));
        }
    }
    let _ = writeln!(out);
    let now = store::now();
    for (b, s) in &hits {
        let _ = writeln!(
            out,
            "ck: {handle} from `{}`: {}, last run {}",
            command_line(b),
            streak(&s.history),
            ago(now.saturating_sub(b.recorded_at))
        );
    }
    0
}

fn here() -> Place {
    Place::discover(&std::env::current_dir().unwrap_or_default())
}

fn split(loaded: Vec<Load>) -> (Vec<Baseline>, Vec<String>) {
    let mut found = Vec::new();
    let mut unusable = Vec::new();
    for load in loaded {
        match load {
            Load::Found(b) => found.push(b),
            Load::Unusable(why) => unusable.push(why),
            Load::Missing => {}
        }
    }
    (found, unusable)
}

/// "on main", "at 0123abcd", "in this directory".
fn where_(place: &Place) -> String {
    match &place.branch {
        Branch::Named(name) => format!("on {name}"),
        Branch::Detached(commit) => format!("at {}", &commit[..commit.len().min(12)]),
        Branch::NoGit => "in this directory".to_string(),
    }
}

fn command_line(b: &Baseline) -> String {
    let c = &b.command;
    let mut words: Vec<String> = c.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    words.extend(c.argv.iter().cloned());
    let mut line = words.join(" ");
    if !c.dir.is_empty() {
        line.push_str(&format!("  (in {})", c.dir));
    }
    line
}

fn counts(b: &Baseline) -> (usize, usize) {
    b.failures
        .iter()
        .filter(|s| s.history.ends_with('F'))
        .fold((0, 0), |(e, w), s| match s.failure.severity {
            crate::report::Severity::Error => (e + 1, w),
            crate::report::Severity::Warning => (e, w + 1),
        })
}

/// "failing for 3 runs" or "not seen for 2 runs", from the history.
fn streak(history: &str) -> String {
    let last = history.chars().last().unwrap_or('F');
    let n = history.chars().rev().take_while(|&c| c == last).count();
    let runs = if n >= Stored::HISTORY {
        format!("{}+ runs", Stored::HISTORY)
    } else {
        format!("{n} {}", plural(n, "run"))
    };
    if last == 'F' {
        format!("failing for {runs}")
    } else {
        format!("not seen for {runs}")
    }
}

fn ago(secs: u64) -> String {
    let (n, unit) = if secs < 60 {
        return "just now".to_string();
    } else if secs < 3600 {
        (secs / 60, "minute")
    } else if secs < 86_400 {
        (secs / 3600, "hour")
    } else {
        (secs / 86_400, "day")
    };
    format!("{n} {} ago", plural(n as usize, unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaks_read_from_the_end_of_the_history() {
        assert_eq!(streak("FFF"), "failing for 3 runs");
        assert_eq!(streak("F"), "failing for 1 run");
        assert_eq!(streak("FFPP"), "not seen for 2 runs");
        assert_eq!(streak("PFPF"), "failing for 1 run");
        assert_eq!(streak(&"F".repeat(40)), "failing for 32+ runs");
    }

    #[test]
    fn ago_rounds_down_to_the_largest_unit() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(59 * 60), "59 minutes ago");
        assert_eq!(ago(2 * 3600 + 100), "2 hours ago");
        assert_eq!(ago(3 * 86_400), "3 days ago");
    }
}
