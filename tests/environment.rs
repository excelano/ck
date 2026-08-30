//! Two shapes of command line that a shell handles and a bare wrapper does
//! not: a command prefixed with variable assignments, and a shell builtin.
//!
//! Both came out of a real session. `ck cd ~/project && ck cargo test` failed
//! at the `cd` and everything after it ran in the wrong place, which is the
//! kind of breakage that gets a tool removed rather than reported.

use std::process::{Command, Stdio};

const CK: &str = env!("CARGO_BIN_EXE_ck");

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let out = Command::new(CK)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ck");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn a_leading_assignment_reaches_the_command() {
    let (stdout, _, code) = run(&["FOO=fromck", "sh", "-c", "echo $FOO"]);
    assert_eq!(stdout, "fromck\n");
    assert_eq!(code, Some(0));
}

#[test]
fn several_assignments_all_reach_the_command() {
    let (stdout, _, _) = run(&["A=1", "B=2", "sh", "-c", "echo $A$B"]);
    assert_eq!(stdout, "12\n");
}

#[test]
fn an_assignment_matches_what_the_shell_would_have_done() {
    let through_ck = run(&["FOO=x", "sh", "-c", "echo ${FOO}"]).0;
    let through_shell = Command::new("sh")
        .args(["-c", "FOO=x sh -c 'echo ${FOO}'"])
        .output()
        .expect("run sh");
    assert_eq!(through_ck, String::from_utf8_lossy(&through_shell.stdout));
}

#[test]
fn the_assignment_does_not_leak_past_the_command() {
    // It belongs to the child, not to ck and not to anything ck runs later.
    let (stdout, _, _) = run(&["CK_LEAK_CHECK=1", "sh", "-c", "echo ${CK_LEAK_CHECK}"]);
    assert_eq!(stdout, "1\n");
    assert!(
        std::env::var("CK_LEAK_CHECK").is_err(),
        "the assignment escaped into ck's own environment"
    );
}

#[test]
fn assignments_with_nothing_to_run_are_refused_not_guessed_at() {
    let (_, stderr, code) = run(&["FOO=1"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("no command"), "got: {stderr}");
}

#[test]
fn the_separator_turns_an_assignment_back_into_a_program_name() {
    let (_, stderr, code) = run(&["--", "FOO=1"]);
    assert_eq!(code, Some(127));
    assert!(stderr.contains("command not found"), "got: {stderr}");
}

#[test]
fn a_shell_builtin_says_why_rather_than_command_not_found() {
    for builtin in ["cd", "export", "source", "ulimit", "alias"] {
        let (_, stderr, code) = run(&[builtin, "whatever"]);
        assert_eq!(code, Some(127), "{builtin}");
        assert!(stderr.contains("shell builtin"), "{builtin} got: {stderr}");
        assert!(
            stderr.contains("without ck"),
            "{builtin} should say what to do instead"
        );
    }
}

#[test]
fn a_builtin_name_that_is_also_a_real_program_still_runs() {
    // `test` and `times` are builtins, but /usr/bin/test exists and the check
    // only fires after an exec has actually failed.
    let (_, _, code) = run(&["test", "1", "=", "1"]);
    assert_eq!(
        code,
        Some(0),
        "a real program shadowed by a builtin name did not run"
    );
}

#[test]
fn an_ordinary_missing_command_keeps_the_plain_message() {
    let (_, stderr, code) = run(&["ck-definitely-not-a-command"]);
    assert_eq!(code, Some(127));
    assert!(stderr.contains("command not found"));
    assert!(!stderr.contains("shell builtin"));
}
