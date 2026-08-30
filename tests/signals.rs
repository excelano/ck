//! Interrupt handling, which is the part of a wrapper most likely to be subtly
//! wrong and least likely to be noticed. The failures being guarded against are
//! an orphaned process group holding a port after a cancelled run, and a
//! verdict invented for a run that never finished.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

const CK: &str = env!("CARGO_BIN_EXE_ck");

/// A running `ck`, its command's process group, and the command's stdin.
struct Running {
    ck: Child,
    pgid: i32,
    /// Held, not read. Dropping it closes `cat`'s stdin, which ends the
    /// command before the test has signalled it.
    _stdin: ChildStdin,
}

/// Run a shell fragment under `ck` and return once the command is provably
/// running, not merely spawned.
///
/// Both halves of that matter. The command reports its own group id — `ck`
/// makes it a group leader, so the shell's `$$` is the group — and then execs
/// `cat`, which the test round-trips a line through. The round-trip is the
/// handshake: it cannot succeed until the exec has completed.
///
/// Without it these tests race the shell's own startup, and the race is real
/// rather than theoretical. A signal delivered in the window between the shell
/// printing its pid and reaching its next command is swallowed by the shell —
/// verified against the bare command, which survives a SIGINT sent at that
/// instant 25 times out of 25. Signalling into that window measures dash, not
/// ck.
fn start(before_cat: &str) -> Running {
    let script = format!("echo $$; {before_cat} exec cat");
    let mut ck = Command::new(CK)
        .args(["sh", "-c", &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ck");

    let mut stdin = ck.stdin.take().expect("piped stdin");
    let mut stdout = BufReader::new(ck.stdout.take().expect("piped stdout"));

    let mut line = String::new();
    stdout
        .read_line(&mut line)
        .expect("read the command's own pid");
    let pgid: i32 = line.trim().parse().expect("a pid on the first line");

    writeln!(stdin, "ready").expect("write the handshake");
    stdin.flush().expect("flush");
    let mut echoed = String::new();
    stdout
        .read_line(&mut echoed)
        .expect("read the handshake back");
    assert_eq!(
        echoed.trim(),
        "ready",
        "the command never reached its blocking state"
    );

    Running {
        ck,
        pgid,
        _stdin: stdin,
    }
}

fn signal_ck(pid: u32, sig: libc::c_int) {
    // SAFETY: pid is a live child of this process.
    unsafe { libc::kill(pid as libc::pid_t, sig) };
}

/// Wait for every process in the group to be gone. Signal 0 checks for the
/// group's existence without sending anything.
fn wait_for_empty(pgid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        // SAFETY: signal 0 only performs error checking.
        if unsafe { libc::kill(-pgid, 0) } == -1 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Each of the three signals a terminal or a supervisor actually sends.
fn forwards_and_reports(sig: libc::c_int) {
    let mut run = start("");
    signal_ck(run.ck.id(), sig);
    let status = run.ck.wait().expect("wait");
    assert_eq!(
        status.code(),
        Some(128 + sig),
        "an interrupted run reports 128+signal"
    );
    assert!(
        wait_for_empty(run.pgid),
        "the command outlived the interrupt"
    );
}

#[test]
fn sigterm_is_forwarded_and_reported() {
    forwards_and_reports(libc::SIGTERM);
}

#[test]
fn sigint_is_forwarded_and_reported() {
    forwards_and_reports(libc::SIGINT);
}

#[test]
fn sighup_is_forwarded_and_reported() {
    forwards_and_reports(libc::SIGHUP);
}

#[test]
fn the_whole_group_goes_not_just_the_direct_child() {
    // A runner's own children are what get orphaned when a kill is not
    // group-level. SIGTERM rather than SIGINT because a non-interactive shell
    // sets SIGINT to SIG_IGN for the jobs it backgrounds — the bare command
    // behaves the same way, and matching it is the promise.
    let mut run = start("sleep 60 &");
    signal_ck(run.ck.id(), libc::SIGTERM);
    run.ck.wait().expect("wait");
    assert!(
        wait_for_empty(run.pgid),
        "a grandchild survived the interrupt"
    );
}

#[test]
fn the_command_runs_in_a_group_of_its_own() {
    let mut run = start("");
    // SAFETY: reading the group of a live child.
    let ck_group = unsafe { libc::getpgid(run.ck.id() as libc::pid_t) };
    assert_ne!(
        run.pgid, ck_group,
        "the command shares ck's group; a group kill would hit ck too"
    );
    signal_ck(run.ck.id(), libc::SIGTERM);
    run.ck.wait().expect("wait");
    wait_for_empty(run.pgid);
}

#[test]
fn an_uninterrupted_run_is_untouched_by_any_of_this() {
    let status = Command::new(CK)
        .args(["sh", "-c", "exit 7"])
        .status()
        .expect("run");
    assert_eq!(status.code(), Some(7));
}
