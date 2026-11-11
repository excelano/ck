//! ck — report what changed in a command's failures since the last run.
//!
//! Author: David M. Anderson
//! Built with AI assistance (Claude, Anthropic)

mod adapter;
mod cli;
mod compare;
mod exec;
mod inspect;
mod raw;
mod render;
mod report;
mod run;
mod store;

use cli::{Invocation, ParseError};

const HELP: &str = "\
ck — report what changed in a command's failures since the last run

USAGE
    ck <command> [args...]
    ck show <id>
    ck

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
    --verify         Shadow mode: the normal output, then what was parsed,
                     then the raw output, so the three can be checked
                     against each other
    -h, --help       Print this message
    -V, --version    Print the version

COMMANDS
    show <id>        The stored detail for one failure, by the id a
                     compared run printed
    (none)           The baselines recorded for this tree and branch

EXIT
    A compared run exits 0 when nothing is new and 1 when a new error
    appeared on a run the command itself failed. A run ck does not compare
    (a command it does not recognize, a first run, output it could not
    fully parse) exits with the command's own code. A command that could
    not be started exits 127 (not found) or 126 (not executable); one
    killed by a signal exits 128 plus the signal number. A usage error
    exits 2.
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
        Ok(Invocation::Status) => inspect::status(),
        Ok(Invocation::Show(handle)) => inspect::show(&handle),
        // On a passthrough there is no verdict for `verify` to show, and the
        // raw output is what passthrough already prints.
        Ok(Invocation::Run { env, argv, verify }) => match adapter::detect(&argv) {
            None => exec::run(&env, &argv),
            Some(matched) => match exec::capture(&env, &matched.argv) {
                Ok(captured) => run::finish(matched.adapter, captured, &env, &argv, verify),
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
