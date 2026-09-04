//! Cargo: `build`, `check`, `clippy`, and `test`.
//!
//! All four are run with `--message-format=json`, which makes cargo write
//! every compiler diagnostic as one JSON line on stdout, followed by a
//! `build-finished` line. Anything the build then runs — a test binary, most
//! usefully — writes to the same stdout after that line, untouched. So one
//! interception covers the compile-fail path of every subcommand, and the
//! test parser slots in behind `build-finished` on the same stream.
//!
//! The flag goes in directly after the subcommand, before any `--`, so it can
//! never land among the arguments cargo forwards to a test binary. A command
//! line that already carries `--message-format` is left alone entirely: the
//! caller asked for a particular shape of output and a wrapper that overrides
//! that is the wrapper that gets bypassed.
//!
//! The parser lives in `diagnostics`; this module owns the command line and
//! the raw replay.
//!
//! Matching is conservative. Cargo's own flags before the subcommand are
//! walked with a fixed list of which ones take a value; an unfamiliar one
//! means the subcommand cannot be found with confidence, and the command
//! passes through. Passthrough is never wrong. A false match — injecting the
//! flag into a command line that was read incorrectly — is.

mod diagnostics;

use crate::exec::{Captured, Stream};
use crate::raw;

pub use diagnostics::parse;

/// The subcommands whose output is compiler diagnostics, with cargo's
/// built-in single-letter aliases.
const SUBCOMMANDS: &[&str] = &["build", "b", "check", "c", "clippy", "test", "t"];

/// Global cargo flags that take their value from the next token, or attached
/// with `=` (`--color=always`) or directly (`-Zunstable-options`).
const GLOBAL_WITH_VALUE: &[&str] = &["--color", "--config", "-Z", "-C"];

/// Global cargo flags that stand alone.
const GLOBAL_BARE: &[&str] = &[
    "-v",
    "-vv",
    "--verbose",
    "-q",
    "--quiet",
    "--locked",
    "--offline",
    "--frozen",
];

/// What gets inserted. `json` rather than one of the rendered variants: the
/// `rendered` field of every message carries the compiler's own text, which
/// is what gets printed on the raw path, so nothing is lost by asking for the
/// plain form.
pub const MESSAGE_FORMAT: &str = "--message-format=json";

/// If `argv` runs one of the recognized cargo subcommands, return it with the
/// JSON message format switched on. `None` means pass through untouched.
pub fn detect(argv: &[String]) -> Option<Vec<String>> {
    let program = std::path::Path::new(argv.first()?).file_name()?;
    if program != "cargo" {
        return None;
    }

    let mut i = 1;
    // `cargo +nightly build`: a rustup toolchain override.
    if argv.get(i).is_some_and(|a| a.starts_with('+')) {
        i += 1;
    }

    loop {
        let tok = argv.get(i)?.as_str();
        if GLOBAL_BARE.contains(&tok) {
            i += 1;
        } else if GLOBAL_WITH_VALUE.contains(&tok) {
            i += 2;
        } else if GLOBAL_WITH_VALUE
            .iter()
            .any(|flag| tok.len() > flag.len() && tok.starts_with(flag))
        {
            i += 1;
        } else if tok.starts_with('-') {
            return None;
        } else {
            break;
        }
    }

    let subcommand = argv.get(i)?.as_str();
    if !SUBCOMMANDS.contains(&subcommand) {
        return None;
    }

    // The subcommand's own flags, up to `--`. Two things mean hands off: the
    // caller chose an output format, or asked for help rather than a build.
    let owned = argv[i + 1..].iter().take_while(|t| *t != "--");
    for tok in owned {
        if tok == "--message-format"
            || tok.starts_with("--message-format=")
            || tok == "-h"
            || tok == "--help"
        {
            return None;
        }
    }

    let mut rewritten = argv.to_vec();
    rewritten.insert(i + 1, MESSAGE_FORMAT.to_string());
    Some(rewritten)
}

/// What one line of the child's stdout is, given where the stream is.
enum Line {
    /// A compiler diagnostic; the text is what cargo would have printed.
    Diagnostic(String),
    /// Cargo's own bookkeeping: artifacts, build scripts, `build-finished`.
    Scaffolding,
    /// Not cargo's. A test binary's output, or something unexpected.
    Text,
}

/// Replay a captured run as cargo's human output. Diagnostics go to stderr,
/// where cargo puts them; cargo's own stderr goes back out verbatim; and
/// whatever followed `build-finished` on stdout — a test binary, usually —
/// goes to stdout untouched. Ordering across the two streams is the order
/// the chunks arrived, which is as close to cargo's own interleaving as a
/// pipe allows.
pub fn replay(captured: &Captured) -> Vec<raw::Line> {
    let mut lines = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut building = true;
    for chunk in &captured.chunks {
        match chunk.stream {
            Stream::Stderr => lines.extend(raw::split(Stream::Stderr, &chunk.bytes)),
            Stream::Stdout => {
                pending.extend_from_slice(&chunk.bytes);
                while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=end).collect();
                    match classify(&line, &mut building) {
                        Line::Diagnostic(text) => {
                            lines.extend(raw::split(Stream::Stderr, text.as_bytes()));
                        }
                        Line::Scaffolding => {}
                        Line::Text => lines.push(raw::Line {
                            stream: Stream::Stdout,
                            bytes: line,
                        }),
                    }
                }
            }
        }
    }
    lines.extend(raw::split(Stream::Stdout, &pending));
    lines
}

/// Decide what a stdout line is. `building` is true until `build-finished`
/// has gone by; after that nothing on stdout is cargo's, however it looks.
fn classify(line: &[u8], building: &mut bool) -> Line {
    if !*building {
        return Line::Text;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return Line::Text;
    };
    match value.get("reason").and_then(|r| r.as_str()) {
        Some("compiler-message") => {
            match value.pointer("/message/rendered").and_then(|r| r.as_str()) {
                Some(rendered) => Line::Diagnostic(rendered.to_string()),
                // A message with no rendering is not something to guess at.
                None => Line::Text,
            }
        }
        Some("build-finished") => {
            *building = false;
            Line::Scaffolding
        }
        Some(_) => Line::Scaffolding,
        None => Line::Text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn rewritten(args: &[&str]) -> Vec<String> {
        detect(&v(args)).unwrap_or_else(|| panic!("{args:?} should match"))
    }

    fn passthrough(args: &[&str]) {
        assert_eq!(detect(&v(args)), None, "{args:?} should pass through");
    }

    #[test]
    fn each_recognized_subcommand_gets_the_flag_directly_after_it() {
        for sub in SUBCOMMANDS {
            assert_eq!(
                rewritten(&["cargo", sub]),
                v(&["cargo", sub, MESSAGE_FORMAT]),
                "{sub}"
            );
        }
    }

    #[test]
    fn the_flag_lands_before_the_subcommands_own_arguments() {
        assert_eq!(
            rewritten(&["cargo", "test", "--release", "--", "--nocapture"]),
            v(&[
                "cargo",
                "test",
                MESSAGE_FORMAT,
                "--release",
                "--",
                "--nocapture"
            ])
        );
    }

    #[test]
    fn other_subcommands_pass_through() {
        for sub in ["run", "fmt", "doc", "bench", "publish", "new"] {
            passthrough(&["cargo", sub]);
        }
    }

    #[test]
    fn only_cargo_matches() {
        passthrough(&["make", "check"]);
        passthrough(&["cargo-clippy"]);
        passthrough(&["rustc", "--error-format=json", "x.rs"]);
    }

    #[test]
    fn cargo_by_path_still_matches() {
        assert_eq!(
            rewritten(&["/usr/bin/cargo", "build"]),
            v(&["/usr/bin/cargo", "build", MESSAGE_FORMAT])
        );
    }

    #[test]
    fn a_toolchain_override_is_skipped() {
        assert_eq!(
            rewritten(&["cargo", "+nightly", "check"]),
            v(&["cargo", "+nightly", "check", MESSAGE_FORMAT])
        );
    }

    #[test]
    fn known_global_flags_are_walked_past() {
        assert_eq!(
            rewritten(&["cargo", "-q", "build"]),
            v(&["cargo", "-q", "build", MESSAGE_FORMAT])
        );
        assert_eq!(
            rewritten(&["cargo", "--color", "always", "build"]),
            v(&["cargo", "--color", "always", "build", MESSAGE_FORMAT])
        );
        assert_eq!(
            rewritten(&["cargo", "--color=always", "build"]),
            v(&["cargo", "--color=always", "build", MESSAGE_FORMAT])
        );
        assert_eq!(
            rewritten(&["cargo", "-Z", "unstable-options", "-Zbuild-std", "build"]),
            v(&[
                "cargo",
                "-Z",
                "unstable-options",
                "-Zbuild-std",
                "build",
                MESSAGE_FORMAT
            ])
        );
    }

    #[test]
    fn an_unfamiliar_global_flag_means_passthrough() {
        // Reading past it would be a guess at where the subcommand is, and a
        // wrong guess injects the flag into the wrong place.
        passthrough(&["cargo", "--something-new", "build"]);
        passthrough(&["cargo", "--list"]);
    }

    #[test]
    fn a_callers_own_message_format_is_respected() {
        passthrough(&["cargo", "build", "--message-format=short"]);
        passthrough(&["cargo", "build", "--message-format", "json"]);
        passthrough(&["cargo", "clippy", "--all-targets", "--message-format=json"]);
    }

    #[test]
    fn arguments_after_the_separator_are_not_inspected() {
        assert_eq!(
            rewritten(&["cargo", "test", "--", "--message-format=x"]),
            v(&["cargo", "test", MESSAGE_FORMAT, "--", "--message-format=x"])
        );
    }

    #[test]
    fn asking_for_help_passes_through() {
        passthrough(&["cargo", "build", "--help"]);
        passthrough(&["cargo", "test", "-h"]);
    }

    #[test]
    fn nothing_to_run_passes_through() {
        passthrough(&["cargo"]);
        passthrough(&["cargo", "+nightly"]);
        passthrough(&["cargo", "-q"]);
        passthrough(&["cargo", "--color"]);
    }

    fn classified(line: &str, building: &mut bool) -> &'static str {
        match classify(line.as_bytes(), building) {
            Line::Diagnostic(_) => "diagnostic",
            Line::Scaffolding => "scaffolding",
            Line::Text => "text",
        }
    }

    #[test]
    fn a_compiler_message_is_replayed_as_its_rendering() {
        let mut building = true;
        let line = r#"{"reason":"compiler-message","message":{"rendered":"error: x\n"}}"#;
        match classify(line.as_bytes(), &mut building) {
            Line::Diagnostic(text) => assert_eq!(text, "error: x\n"),
            _ => panic!("should be a diagnostic"),
        }
    }

    #[test]
    fn cargos_bookkeeping_is_dropped() {
        let mut building = true;
        assert_eq!(
            classified(
                r#"{"reason":"compiler-artifact","target":{}}"#,
                &mut building
            ),
            "scaffolding"
        );
        assert_eq!(
            classified(r#"{"reason":"build-script-executed"}"#, &mut building),
            "scaffolding"
        );
    }

    #[test]
    fn nothing_after_build_finished_is_cargos() {
        let mut building = true;
        assert_eq!(
            classified(
                r#"{"reason":"build-finished","success":true}"#,
                &mut building
            ),
            "scaffolding"
        );
        assert!(!building);
        assert_eq!(classified("running 3 tests", &mut building), "text");
        // Even a line that looks like cargo's.
        assert_eq!(
            classified(r#"{"reason":"compiler-artifact"}"#, &mut building),
            "text"
        );
    }

    #[test]
    fn lines_that_are_not_cargos_are_text() {
        let mut building = true;
        assert_eq!(classified("plain output", &mut building), "text");
        assert_eq!(classified(r#"{"not":"cargo"}"#, &mut building), "text");
        assert_eq!(
            classified(
                r#"{"reason":"compiler-message","message":{}}"#,
                &mut building
            ),
            "text"
        );
    }
}
