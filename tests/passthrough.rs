//! The wrapper's one promise, tested directly: running a command through `ck`
//! produces the same bytes on the same streams and the same exit code as
//! running it bare.
//!
//! This is a differential test rather than a set of expectations, because the
//! property is equality with the raw command, not agreement with a transcript
//! someone wrote down. A wrapper that silently swallows output is the failure
//! that kills adoption, and it is invisible to any test that only checks that
//! output looks right.

use std::io::Write;
use std::process::{Command, Stdio};

const CK: &str = env!("CARGO_BIN_EXE_ck");

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    code: Option<i32>,
}

fn execute(program: &str, args: &[&str], stdin: Option<&[u8]>) -> Outcome {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    if let Some(bytes) = stdin {
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(bytes)
            .expect("write stdin");
    }
    let out = child.wait_with_output().expect("wait");
    Outcome {
        stdout: out.stdout,
        stderr: out.stderr,
        code: out.status.code(),
    }
}

/// Run the same command bare and through `ck`, and require the two to be
/// indistinguishable.
fn assert_transparent(args: &[&str], stdin: Option<&[u8]>) {
    let bare = execute(args[0], &args[1..], stdin);
    let wrapped = execute(CK, args, stdin);
    assert_eq!(bare, wrapped, "ck changed the result of {args:?}");
}

fn sh(script: &str) -> Vec<&str> {
    vec!["sh", "-c", script]
}

#[test]
fn stdout_passes_through() {
    assert_transparent(&sh("echo hello"), None);
}

#[test]
fn stderr_passes_through_and_stays_on_stderr() {
    assert_transparent(&sh("echo problem >&2"), None);
}

#[test]
fn both_streams_stay_separate() {
    assert_transparent(&sh("echo out; echo err >&2"), None);
}

#[test]
fn output_without_a_trailing_newline_is_not_repaired() {
    assert_transparent(&sh("printf 'no trailing newline'"), None);
}

#[test]
fn non_utf8_bytes_survive() {
    assert_transparent(&sh(r"printf '\001\002\377\000end'"), None);
}

#[test]
fn large_output_is_not_truncated_or_reordered() {
    assert_transparent(&sh("seq 1 200000"), None);
}

#[test]
fn stdin_reaches_the_command() {
    assert_transparent(&sh("cat"), Some(b"fed through the wrapper\n"));
}

#[test]
fn exit_codes_are_preserved() {
    for code in [0, 1, 2, 3, 42, 125, 255] {
        assert_transparent(&sh(&format!("exit {code}")), None);
    }
}

#[test]
fn a_successful_command_has_nothing_added_to_it() {
    let wrapped = execute(CK, &["true"], None);
    assert_eq!(
        wrapped.stdout, b"",
        "ck wrote to stdout on a silent success"
    );
    assert_eq!(
        wrapped.stderr, b"",
        "ck wrote to stderr on a silent success"
    );
    assert_eq!(wrapped.code, Some(0));
}

#[test]
fn a_missing_command_is_not_a_clean_run() {
    let out = execute(CK, &["ck-no-such-command-exists"], None);
    assert_eq!(out.code, Some(127));
    assert!(String::from_utf8_lossy(&out.stderr).contains("command not found"));
    assert_eq!(out.stdout, b"");
}

#[test]
fn a_non_executable_file_is_not_a_clean_run() {
    let dir = std::env::temp_dir().join(format!("ck-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("not-executable");
    std::fs::write(&path, b"#!/bin/sh\necho nope\n").expect("write");
    let out = execute(CK, &[path.to_str().expect("path")], None);
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(out.code, Some(126));
    assert!(String::from_utf8_lossy(&out.stderr).contains("permission denied"));
}

#[test]
fn the_separator_is_optional_end_to_end() {
    let without = execute(CK, &["echo", "same"], None);
    let with = execute(CK, &["--", "echo", "same"], None);
    assert_eq!(without, with);
    assert_eq!(without.stdout, b"same\n");
}

#[test]
fn the_commands_own_flags_are_never_claimed_by_ck() {
    // -n belongs to echo. If ck tried to interpret it, this would fail or error.
    assert_transparent(&["echo", "-n", "flagless"], None);
}

#[test]
fn version_and_help_answer_without_running_anything() {
    let version = execute(CK, &["--version"], None);
    assert_eq!(version.code, Some(0));
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("ck "));

    for flag in ["-h", "--help"] {
        let help = execute(CK, &[flag], None);
        assert_eq!(help.code, Some(0));
        assert!(String::from_utf8_lossy(&help.stdout).contains("USAGE"));
    }
}

#[test]
fn an_unknown_leading_flag_is_refused_rather_than_forwarded() {
    let out = execute(CK, &["--not-a-ck-option", "true"], None);
    assert_eq!(out.code, Some(2));
    let msg = String::from_utf8_lossy(&out.stderr);
    assert!(msg.contains("unknown option"));
    assert!(msg.contains("--"), "the error should show the escape");
}
