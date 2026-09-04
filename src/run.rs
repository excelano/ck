//! One captured run, from the child's exit status to ck's exit code.
//!
//! The order of decisions is the exit-code table in `DESIGN.md`, read top
//! to bottom. A signal means no verdict at all. A parse that is not clean
//! means the raw output is the answer. A failed child with no error found
//! means the parse missed what failed, and comparing would let ck say
//! "nothing new" about a failure it never saw. Only then is there a
//! baseline to consult, and only then does the gate decide the code.
//!
//! The baseline is written before anything is printed, so that a caller
//! closing the pipe early cannot cost the next run its comparison.

use std::io::Write;
use std::os::unix::process::ExitStatusExt;

use crate::adapter::{self, AdapterId};
use crate::compare::{self, Comparison};
use crate::exec::{self, Captured};
use crate::raw;
use crate::render;
use crate::report::{Parse, RunReport};
use crate::store::repo::Place;
use crate::store::{self, Key, Load, Skipped, Slot};

/// Finish a run whose output was captured. `env` and `argv` are the
/// command as the caller wrote it, which is what the baseline is keyed on.
pub fn finish(
    adapter: AdapterId,
    captured: Captured,
    env: &[(String, String)],
    argv: &[String],
    verify: bool,
) -> i32 {
    // A current directory that cannot be read is a directory that has been
    // deleted underneath the shell; the command is about to say so itself.
    let cwd = std::env::current_dir().unwrap_or_default();
    let key = Key::new(Place::discover(&cwd), env, argv);
    let log = store::log_path(&key);
    let dump = |captured: &Captured| raw::emit(&adapter::replay(adapter, captured), log.as_deref());

    // An interrupted run produces no verdict: whatever was captured, then
    // 128 + signal. A baseline missing everything that never ran would
    // report all of it as fixed next time.
    if captured.status.signal().is_some() {
        dump(&captured);
        return exec::exit_code(&captured.status);
    }
    let code = exec::exit_code(&captured.status);
    let report = adapter::parse(adapter, captured);

    let compared = if report.parse != Parse::Clean {
        None
    } else if compare::unaccounted(&report, code) {
        eprintln!(
            "ck: the command exited {code} but no error was found in its output; \
             baseline left as it was"
        );
        None
    } else {
        Some(against_baseline(&report, key, code))
    };

    let mut out = std::io::stdout().lock();
    let exit = match &compared {
        None => {
            if !verify {
                drop(out);
                dump(&report.raw);
                out = std::io::stdout().lock();
            }
            code
        }
        Some((_, true)) => {
            // First contact prints what the bare command would have, plus
            // one line. In shadow mode the raw output comes at the end.
            if !verify {
                drop(out);
                dump(&report.raw);
                out = std::io::stdout().lock();
            }
            let _ = render::first_contact(&mut out, &report);
            let _ = out.flush();
            code
        }
        Some((comparison, false)) => {
            let _ = render::comparison(&mut out, comparison, report.totals);
            let _ = out.flush();
            compare::gate(comparison, code)
        }
    };

    if verify {
        let _ = render::parsed(&mut out, &report, code);
        let _ = writeln!(out, "{}", render::RAW_SEPARATOR);
        let _ = out.flush();
        drop(out);
        dump(&report.raw);
    }
    exit
}

/// Compare against the stored baseline and advance it. The second value is
/// whether this was first contact.
fn against_baseline(report: &RunReport, key: Key, code: i32) -> (Comparison, bool) {
    let slot = Slot::open(&key);
    let previous = match slot.load() {
        Load::Found(baseline) => Some(baseline),
        Load::Missing => None,
        Load::Unusable(why) => {
            eprintln!("ck: {why}; starting over");
            None
        }
    };
    let first_contact = previous.is_none();
    let comparison = compare::compare(previous, report, key.command, code);

    match slot.save(&comparison.next) {
        Ok(()) => {}
        Err(Skipped::Contended) => {
            eprintln!("ck: another ck is running this command; baseline left as it was");
        }
        Err(Skipped::NoStore) => {
            eprintln!(
                "ck: nowhere to keep a baseline; set {} or HOME",
                store::ROOT_VAR
            );
        }
        Err(Skipped::Io(e)) => eprintln!("ck: baseline not written: {e}"),
    }
    (comparison, first_contact)
}
