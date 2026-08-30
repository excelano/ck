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
    /// Retrieve suppressed detail for one failure.
    Show(String),
    /// Run this command line, verbatim.
    Run(Vec<String>),
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
        }
    }
}

/// Parse `ck`'s own arguments, excluding argv[0].
///
/// Every branch here is terminal, because every option `ck` currently defines
/// either answers immediately or hands the rest of the line to the command.
/// The first option that does neither turns this into a loop over the leading
/// flags; nothing else about the grammar changes.
pub fn parse(args: &[String]) -> Result<Invocation, ParseError> {
    let Some(first) = args.first().map(String::as_str) else {
        // Bare `ck`. Reports what it can rather than erroring; once a baseline
        // store exists this should report the baseline for the current repo
        // and branch instead of the help text.
        return Ok(Invocation::Help);
    };

    // Everything after `--` is the command, untouched.
    if first == "--" {
        let rest = &args[1..];
        return if rest.is_empty() {
            Err(ParseError::EmptyAfterSeparator)
        } else {
            Ok(Invocation::Run(rest.to_vec()))
        };
    }

    // A lone "-" is a conventional filename, not a flag.
    if first.starts_with('-') && first != "-" {
        return match first {
            "-h" | "--help" => Ok(Invocation::Help),
            "-V" | "--version" => Ok(Invocation::Version),
            other => Err(ParseError::UnknownFlag(other.to_string())),
        };
    }

    // First non-flag token. Either the one reserved word, or the command.
    if first == "show" {
        return match args.get(1) {
            Some(id) => Ok(Invocation::Show(id.clone())),
            None => Err(ParseError::ShowNeedsIdentity),
        };
    }

    Ok(Invocation::Run(args.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }
    fn run(args: &[&str]) -> Invocation {
        Invocation::Run(v(args))
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
        assert_eq!(parse(&[]).unwrap(), Invocation::Help);
    }

    #[test]
    fn separator_with_nothing_after_it_is_an_error() {
        assert_eq!(parse(&v(&["--"])), Err(ParseError::EmptyAfterSeparator));
    }
}
