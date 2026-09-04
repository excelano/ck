//! ck — report what changed in a command's failures since the last run.
//!
//! Author: David M. Anderson. Written with AI assistance (Claude Code).
//!
//! This build is the transparent wrapper and nothing more: it runs what you
//! give it and gets out of the way. Comparison against a baseline arrives on
//! top of this, never in place of it — a wrapper that understands some runners
//! well, not a test tool that happens to wrap commands.

mod adapter;
mod cli;
// Built and tested ahead of the run path that calls it; the allowance goes
// with the next step.
#[allow(dead_code)]
mod compare;
mod exec;
mod render;
mod report;
#[allow(dead_code)]
mod store;

use cli::{Invocation, ParseError};

const HELP: &str = "\
ck — report what changed in a command's failures since the last run

USAGE
    ck <command> [args...]

    The first non-flag token begins the command. Everything from there is
    passed through untouched, including its own flags.

        ck cargo test
        ck cargo clippy --all-targets
        ck make check

    Leading NAME=VALUE tokens are added to the command's environment, as a
    shell would.

        ck RUST_BACKTRACE=1 cargo test

    Shell builtins — cd, export, source and the rest — have no executable
    behind them and cannot be wrapped. Run those without ck.

    -- is accepted but never required. Use it when the command itself begins
    with a flag, or when its name is a word ck reserves.

        ck -- -x script.sh
        ck -- show something

OPTIONS
    --verify         Shadow mode: print the verdict and the raw output
                     together, so the two can be checked against each other
    -h, --help       Print this message
    -V, --version    Print the version

COMMANDS
    show <identity>  Retrieve suppressed detail for one failure

EXIT
    ck exits with the wrapped command's own exit code. A command that could
    not be started exits 127 (not found) or 126 (not executable); one killed
    by a signal exits 128 plus the signal number.
";

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let code = match cli::parse(&args) {
        Ok(Invocation::Help) => {
            print!("{HELP}");
            0
        }
        Ok(Invocation::Version) => {
            println!("ck {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Ok(Invocation::Show(_)) => {
            // Reserved in the grammar from the start so it never has to change,
            // but there is nothing to retrieve until failures are being stored.
            eprintln!("ck: show needs a baseline store, which this build does not have");
            2
        }
        // On a passthrough there is no verdict for `verify` to show, and the
        // raw output is what passthrough already prints.
        Ok(Invocation::Run { env, argv, verify }) => match adapter::detect(&argv) {
            None => exec::run(&env, &argv),
            Some(matched) => match exec::capture(&env, &matched.argv) {
                Ok(captured) if verify => {
                    let report = adapter::parse(matched.adapter, captured);
                    render::verify(&report)
                }
                Ok(captured) => {
                    // Every run is first contact until there is a baseline
                    // to compare against, and first contact prints what the
                    // bare command would have. An interrupted run takes the
                    // same path: whatever was captured, then 128 + signal.
                    adapter::dump_raw(matched.adapter, &captured);
                    exec::exit_code(&captured.status)
                }
                Err(code) => code,
            },
        },
        Err(e @ ParseError::UnknownFlag(_)) => usage_error(&e),
        Err(e) => usage_error(&e),
    };

    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// ck's own failures are only possible before the child is spawned, which is
/// what keeps exit 2 from ever being confused with a command's own 2.
fn usage_error(e: &ParseError) -> i32 {
    eprintln!("ck: {e}");
    2
}
