//! Argument parsing.
//!
//! The grammar is deliberately tiny, because the caller should never have to
//! recall it: the first non-flag token begins the wrapped command, and
//! everything from there is passed through verbatim. `--` is accepted but
//! never required, as the escape for a command whose own first token is a flag
//! or whose name collides with a reserved word.

/// What the caller asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Version,
    /// Bare `ck`: report the baselines for the current tree and branch.
    Status,
    /// Retrieve suppressed detail for one failure.
    Show(String),
    /// Run this command line, verbatim, with these variables added to its
    /// environment.
    Run {
        env: Vec<(String, String)>,
        argv: Vec<String>,
        /// Shadow mode: print the verdict and the raw output together, so a
        /// parse that lies is visible in the same screenful.
        verify: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// A flag before the command that `ck` does not define. Never guessed at,
    /// never forwarded: forwarding it silently would make `ck cmd` and `cmd`
    /// disagree about who owns the flag.
    UnknownFlag(String),
    /// `--` with nothing after it.
    EmptyAfterSeparator,
    /// `show` with no identity.
    ShowNeedsIdentity,
    /// `ck FOO=1` with nothing to run. A shell would set the variable in
    /// itself; a wrapper has no such thing to set.
    AssignmentsWithoutCommand,
    /// A flag that modifies a run, with no run to modify.
    FlagWithoutCommand(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Principle 7: the error is a return value. Say what was wrong,
            // then both ways to write what they probably meant.
            Self::UnknownFlag(flag) => write!(
                f,
                "unknown option '{flag}'\n\
                 \n\
                 Options before the command belong to ck, and ck does not define this one.\n\
                 If it belongs to the command you are wrapping, move it after the command:\n\
                 \n    ck <command> {flag}\n\
                 \n\
                 If the command itself begins with a flag, separate it with --:\n\
                 \n    ck -- {flag} ..."
            ),
            Self::EmptyAfterSeparator => write!(f, "-- with no command after it"),
            Self::ShowNeedsIdentity => write!(
                f,
                "show needs a failure identity\n\
                 \n    ck show <identity>\n\
                 \n\
                 To run a command actually named 'show', separate it with --:\n\
                 \n    ck -- show ..."
            ),
            Self::AssignmentsWithoutCommand => write!(
                f,
                "variable assignments with no command to run\n\
                 \n\
                 ck can add variables to a command's environment, but it cannot set\n\
                 them in your shell the way a bare assignment does. Run that without ck."
            ),
            Self::FlagWithoutCommand(flag) => write!(
                f,
                "{flag} needs a command to run\n\
                 \n    ck {flag} <command> [args...]"
            ),
        }
    }
}

/// Parse `ck`'s own arguments, excluding argv[0].
///
/// Flags that belong to `ck` sit before the command and are consumed here.
/// Most of them answer immediately; `--verify` modifies the run and so the
/// loop continues past it to whatever follows.
pub fn parse(args: &[String]) -> Result<Invocation, ParseError> {
    let mut verify = false;
    let mut rest = args;

    loop {
        let Some(first) = rest.first().map(String::as_str) else {
            // Bare `ck` reports what it knows rather than erroring, per the
            // no-argument convention. A flag with nothing after it is a
            // different thing, and an error.
            return if verify {
                Err(ParseError::FlagWithoutCommand("--verify".into()))
            } else {
                Ok(Invocation::Status)
            };
        };

        // Everything after `--` is the command, untouched. Untouched includes
        // not reading leading assignments: `--` means stop interpreting, so
        // `ck -- FOO=1 x` looks for a program actually named `FOO=1`.
        if first == "--" {
            let argv = &rest[1..];
            return if argv.is_empty() {
                Err(ParseError::EmptyAfterSeparator)
            } else {
                Ok(Invocation::Run {
                    env: Vec::new(),
                    argv: argv.to_vec(),
                    verify,
                })
            };
        }

        // A lone "-" is a conventional filename, not a flag.
        if first.starts_with('-') && first != "-" {
            match first {
                "-h" | "--help" => return Ok(Invocation::Help),
                "-V" | "--version" => return Ok(Invocation::Version),
                "--verify" => {
                    verify = true;
                    rest = &rest[1..];
                    continue;
                }
                other => return Err(ParseError::UnknownFlag(other.to_string())),
            }
        }

        break;
    }

    // First non-flag token. Either the one reserved word, or the command.
    if rest[0] == "show" {
        return match rest.get(1) {
            Some(id) => Ok(Invocation::Show(id.clone())),
            None => Err(ParseError::ShowNeedsIdentity),
        };
    }

    run_from(rest, verify)
}

/// Build a run out of a command line, peeling off any leading `NAME=VALUE`
/// assignments the way a shell would.
///
/// `ck FOO=1 cargo test` has to work, because prefixing a command with a
/// variable is ordinary shell writing and an agent produces it without
/// thinking. Without this the assignment is treated as the program name and
/// the run dies with "command not found".
fn run_from(args: &[String], verify: bool) -> Result<Invocation, ParseError> {
    let mut env = Vec::new();
    let mut rest = args;
    while let Some((name, value)) = rest.first().and_then(|a| split_assignment(a)) {
        env.push((name, value));
        rest = &rest[1..];
    }
    if rest.is_empty() {
        return Err(ParseError::AssignmentsWithoutCommand);
    }
    Ok(Invocation::Run {
        env,
        argv: rest.to_vec(),
        verify,
    })
}

/// Split `NAME=VALUE` when `NAME` is a shell-legal variable name. The value
/// keeps everything after the first `=`, so `FOO=a=b` sets FOO to `a=b`.
fn split_assignment(arg: &str) -> Option<(String, String)> {
    let (name, value) = arg.split_once('=')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((name.to_string(), value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }
    fn run(args: &[&str]) -> Invocation {
        Invocation::Run {
            env: Vec::new(),
            argv: v(args),
            verify: false,
        }
    }

    fn run_with(env: &[(&str, &str)], args: &[&str]) -> Invocation {
        Invocation::Run {
            env: env
                .iter()
                .map(|(k, val)| ((*k).to_string(), (*val).to_string()))
                .collect(),
            argv: v(args),
            verify: false,
        }
    }

    fn verified(args: &[&str]) -> Invocation {
        Invocation::Run {
            env: Vec::new(),
            argv: v(args),
            verify: true,
        }
    }

    #[test]
    fn first_non_flag_token_starts_the_command() {
        assert_eq!(
            parse(&v(&["cargo", "test"])).unwrap(),
            run(&["cargo", "test"])
        );
    }

    #[test]
    fn the_commands_own_flags_are_not_inspected() {
        assert_eq!(
            parse(&v(&["cargo", "test", "--release", "--", "--nocapture"])).unwrap(),
            run(&["cargo", "test", "--release", "--", "--nocapture"])
        );
    }

    #[test]
    fn separator_is_optional_but_accepted() {
        assert_eq!(
            parse(&v(&["--", "cargo", "test"])).unwrap(),
            run(&["cargo", "test"])
        );
    }

    #[test]
    fn separator_escapes_a_command_starting_with_a_flag() {
        assert_eq!(
            parse(&v(&["--", "-x", "foo"])).unwrap(),
            run(&["-x", "foo"])
        );
    }

    #[test]
    fn separator_escapes_the_reserved_word() {
        assert_eq!(
            parse(&v(&["--", "show", "x"])).unwrap(),
            run(&["show", "x"])
        );
    }

    #[test]
    fn show_is_reserved_as_a_first_token() {
        assert_eq!(
            parse(&v(&["show", "abc123"])).unwrap(),
            Invocation::Show("abc123".into())
        );
    }

    #[test]
    fn show_without_an_identity_is_an_error() {
        assert_eq!(parse(&v(&["show"])), Err(ParseError::ShowNeedsIdentity));
    }

    #[test]
    fn help_and_version_in_both_spellings() {
        for a in ["-h", "--help"] {
            assert_eq!(parse(&v(&[a])).unwrap(), Invocation::Help);
        }
        for a in ["-V", "--version"] {
            assert_eq!(parse(&v(&[a])).unwrap(), Invocation::Version);
        }
    }

    #[test]
    fn an_unknown_leading_flag_is_refused_not_forwarded() {
        assert_eq!(
            parse(&v(&["--release", "cargo", "build"])),
            Err(ParseError::UnknownFlag("--release".into()))
        );
    }

    #[test]
    fn a_lone_dash_is_a_command_not_a_flag() {
        assert_eq!(parse(&v(&["-", "x"])).unwrap(), run(&["-", "x"]));
    }

    #[test]
    fn bare_invocation_reports_rather_than_errors() {
        assert_eq!(parse(&[]).unwrap(), Invocation::Status);
    }

    #[test]
    fn a_leading_assignment_becomes_environment_not_a_program_name() {
        assert_eq!(
            parse(&v(&["FOO=1", "cargo", "test"])).unwrap(),
            run_with(&[("FOO", "1")], &["cargo", "test"])
        );
    }

    #[test]
    fn several_assignments_are_all_peeled_off() {
        assert_eq!(
            parse(&v(&["A=1", "B=2", "make"])).unwrap(),
            run_with(&[("A", "1"), ("B", "2")], &["make"])
        );
    }

    #[test]
    fn a_value_may_contain_equals_signs() {
        assert_eq!(
            parse(&v(&["RUSTFLAGS=--cfg=x", "cargo", "build"])).unwrap(),
            run_with(&[("RUSTFLAGS", "--cfg=x")], &["cargo", "build"])
        );
    }

    #[test]
    fn an_empty_value_is_still_an_assignment() {
        assert_eq!(
            parse(&v(&["FOO=", "env"])).unwrap(),
            run_with(&[("FOO", "")], &["env"])
        );
    }

    #[test]
    fn assignments_stop_at_the_first_real_token() {
        // The second looks like an assignment but belongs to the command.
        assert_eq!(
            parse(&v(&["A=1", "env", "B=2"])).unwrap(),
            run_with(&[("A", "1")], &["env", "B=2"])
        );
    }

    #[test]
    fn a_token_that_only_looks_like_an_assignment_is_a_program() {
        // Not shell-legal variable names, so they name programs instead.
        for arg in ["./x=y", "1BAD=2", "=noname", "a-b=c"] {
            assert_eq!(parse(&v(&[arg, "z"])).unwrap(), run(&[arg, "z"]), "{arg}");
        }
    }

    #[test]
    fn assignments_with_nothing_to_run_are_refused() {
        assert_eq!(
            parse(&v(&["FOO=1"])),
            Err(ParseError::AssignmentsWithoutCommand)
        );
        assert_eq!(
            parse(&v(&["A=1", "B=2"])),
            Err(ParseError::AssignmentsWithoutCommand)
        );
    }

    #[test]
    fn the_separator_stops_assignment_parsing_too() {
        assert_eq!(
            parse(&v(&["--", "FOO=1", "x"])).unwrap(),
            run(&["FOO=1", "x"])
        );
    }

    #[test]
    fn separator_with_nothing_after_it_is_an_error() {
        assert_eq!(parse(&v(&["--"])), Err(ParseError::EmptyAfterSeparator));
    }

    #[test]
    fn verify_sits_before_the_command_and_is_consumed() {
        assert_eq!(
            parse(&v(&["--verify", "cargo", "test"])).unwrap(),
            verified(&["cargo", "test"])
        );
    }

    #[test]
    fn verify_composes_with_the_separator_and_assignments() {
        assert_eq!(
            parse(&v(&["--verify", "--", "-x", "foo"])).unwrap(),
            verified(&["-x", "foo"])
        );
        assert_eq!(
            parse(&v(&["--verify", "FOO=1", "cargo", "test"])).unwrap(),
            Invocation::Run {
                env: vec![("FOO".into(), "1".into())],
                argv: v(&["cargo", "test"]),
                verify: true,
            }
        );
    }

    #[test]
    fn verify_after_the_command_belongs_to_the_command() {
        assert_eq!(
            parse(&v(&["cargo", "test", "--verify"])).unwrap(),
            run(&["cargo", "test", "--verify"])
        );
    }

    #[test]
    fn verify_with_nothing_to_run_is_an_error() {
        assert_eq!(
            parse(&v(&["--verify"])),
            Err(ParseError::FlagWithoutCommand("--verify".into()))
        );
    }

    #[test]
    fn help_still_wins_after_verify() {
        assert_eq!(
            parse(&v(&["--verify", "--help"])).unwrap(),
            Invocation::Help
        );
    }
}
