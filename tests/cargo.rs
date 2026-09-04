//! The cargo path, tested the same way as passthrough: run a build bare and
//! through `ck`, and require the two to read the same. Cargo is switched into
//! its JSON mode underneath, so this is the test that the replay reproduces
//! cargo's own output rather than something that merely resembles it.
//!
//! Each test builds a throwaway crate of its own, with its own baseline
//! store beside it, so nothing here touches the real cache. Runs are
//! serialised because cargo's package-cache lock announces itself on stderr
//! when contended, and that line would appear in one run and not the other.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const CK: &str = env!("CARGO_BIN_EXE_ck");

static SERIAL: Mutex<()> = Mutex::new(());
static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A crate that exists only for the duration of one test.
struct Probe {
    dir: PathBuf,
}

impl Probe {
    fn with_lib(lib_rs: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ck-probe-{}-{n}", std::process::id()));
        fs::create_dir_all(dir.join("src")).expect("create probe");
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
        )
        .expect("write manifest");
        let probe = Self { dir };
        probe.set_lib(lib_rs);
        probe
    }

    /// Rewriting the file moves its mtime, which is what makes cargo build
    /// again rather than report `Finished` from the cache.
    fn set_lib(&self, lib_rs: &str) {
        fs::write(self.dir.join("src/lib.rs"), lib_rs).expect("write lib");
        std::thread::sleep(Duration::from_millis(20));
    }

    fn command(&self, program: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&self.dir)
            .env("CARGO_TARGET_DIR", self.dir.join("target"))
            .env("CARGO_TERM_COLOR", "never")
            .env("CK_CACHE_DIR", self.dir.join("cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn run(&self, program: &str, args: &[&str]) -> Outcome {
        let out = self.command(program, args).output().expect("run");
        Outcome {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            code: out.status.code(),
        }
    }

    /// The same cargo invocation bare and through `ck`, both from a fresh
    /// mtime so both actually compile, normalised so they can be compared.
    fn both(&self, lib_rs: &str, cargo_args: &[&str]) -> (Outcome, Outcome) {
        let _serial = SERIAL.lock().unwrap();
        self.set_lib(lib_rs);
        let bare = self.run("cargo", cargo_args);
        let wrapped = self.wrapped(lib_rs, cargo_args);
        (bare.normalised(), wrapped.normalised())
    }

    /// One run through `ck`, from a fresh mtime.
    fn wrapped(&self, lib_rs: &str, cargo_args: &[&str]) -> Outcome {
        self.set_lib(lib_rs);
        let mut ck_args = vec!["cargo"];
        ck_args.extend_from_slice(cargo_args);
        self.run(CK, &ck_args)
    }

    /// Pretend to be a checkout on `branch`. Only `HEAD` is read.
    fn on_branch(&self, branch: &str) {
        fs::create_dir_all(self.dir.join(".git")).unwrap();
        fs::write(
            self.dir.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .unwrap();
    }

    fn baselines(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, into: &mut Vec<PathBuf>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, into);
                } else if path.extension().is_some_and(|e| e == "json") {
                    into.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.dir.join("cache"), &mut found);
        found
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Outcome {
    fn normalised(&self) -> Outcome {
        Outcome {
            stdout: normalise(&self.stdout),
            stderr: normalise(&self.stderr),
            code: self.code,
        }
    }

    /// Split off the one line `ck` adds on first contact, so what is left
    /// can be compared with the bare run.
    fn without_trailer(&self) -> (Outcome, String) {
        let body = self.stdout.trim_end_matches('\n');
        let (body, trailer) = match body.rfind('\n') {
            Some(i) if body[i + 1..].starts_with("ck: ") => (&body[..=i], &body[i + 1..]),
            None if body.starts_with("ck: ") => ("", body),
            _ => panic!("no ck trailer in {:?}", self.stdout),
        };
        (
            Outcome {
                stdout: body.to_string(),
                stderr: self.stderr.clone(),
                code: self.code,
            },
            trailer.to_string(),
        )
    }
}

/// Remove what legitimately differs between two runs of the same build.
///
/// Elapsed times (`0.42s`) and libtest's thread ids (`(110692)`) change every
/// run. The per-crate summary that cargo prints in human mode only —
/// `warning: \`probe\` (lib) generated 1 warning` — is a line cargo composes
/// itself and does not emit in JSON mode; the diagnostics it counts are
/// reproduced in full, and the count is what `ck`'s own verdict carries.
/// Only the runner's output goes through here: ck's own `NEW (1)` would
/// read as a thread id.
fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line.starts_with("warning: `") && line.contains(" generated ") {
            continue;
        }
        for (i, word) in line.split(' ').enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let trimmed = word.trim_end_matches(['\n', ',']);
            let is_duration = trimmed.strip_suffix('s').is_some_and(|t| {
                t.contains('.') && t.chars().all(|c| c.is_ascii_digit() || c == '.')
            });
            let is_thread_id = trimmed
                .strip_prefix('(')
                .and_then(|t| t.strip_suffix(')'))
                .is_some_and(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()));
            if is_duration {
                out.push_str("<time>");
                out.push_str(&word[trimmed.len()..]);
            } else if is_thread_id {
                out.push_str("(<id>)");
                out.push_str(&word[trimmed.len()..]);
            } else {
                out.push_str(word);
            }
        }
    }
    out
}

const GREEN: &str = "pub fn answer() -> u32 { 42 }\n";

const BROKEN: &str = "pub fn answer() -> u32 { missing() }\n";

const WARNING: &str = "pub fn answer() -> u32 { let unused = 1; 42 }\n";

const FAILING_TEST: &str = "\
pub fn answer() -> u32 { 41 }

#[cfg(test)]
mod tests {
    #[test]
    fn is_forty_two() { assert_eq!(super::answer(), 42); }
    #[test]
    fn is_positive() { assert!(super::answer() > 0); }
}
";

#[test]
fn a_green_build_reads_the_same_on_first_contact() {
    let probe = Probe::with_lib(GREEN);
    let (bare, wrapped) = probe.both(GREEN, &["build"]);
    let (raw, trailer) = wrapped.without_trailer();
    assert_eq!(bare, raw);
    assert_eq!(bare.code, Some(0));
    assert_eq!(
        trailer,
        "ck: baseline recorded (0 errors, 0 warnings); the next run reports what changed"
    );
}

#[test]
fn a_broken_build_reads_the_same_and_fails_the_same_on_first_contact() {
    let probe = Probe::with_lib(BROKEN);
    let (bare, wrapped) = probe.both(BROKEN, &["build"]);
    let (raw, trailer) = wrapped.without_trailer();
    assert_eq!(bare, raw);
    assert_eq!(bare.code, Some(101));
    assert!(bare.stderr.contains("cannot find function `missing`"));
    assert!(trailer.starts_with("ck: baseline recorded (1 error, 0 warnings)"));
}

#[test]
fn warnings_read_the_same_and_still_succeed_on_first_contact() {
    let probe = Probe::with_lib(WARNING);
    let (bare, wrapped) = probe.both(WARNING, &["check"]);
    let (raw, trailer) = wrapped.without_trailer();
    assert_eq!(bare, raw);
    assert_eq!(bare.code, Some(0));
    assert!(bare.stderr.contains("unused variable"));
    assert!(trailer.starts_with("ck: baseline recorded (0 errors, 1 warning)"));
}

#[test]
fn test_output_stays_on_stdout_after_the_build() {
    // One thread, so libtest reports the tests in the same order both times.
    let probe = Probe::with_lib(FAILING_TEST);
    let (bare, wrapped) = probe.both(FAILING_TEST, &["test", "--", "--test-threads=1"]);
    assert_eq!(bare, wrapped);
    assert_eq!(bare.code, Some(101));
    assert!(bare.stdout.contains("test tests::is_forty_two ... FAILED"));
    assert!(
        bare.stdout
            .contains("test result: FAILED. 1 passed; 1 failed")
    );
}

#[test]
fn a_callers_own_message_format_is_left_alone() {
    // Not matched, so this is passthrough; the JSON reaches the caller.
    let probe = Probe::with_lib(GREEN);
    let (bare, wrapped) = probe.both(GREEN, &["build", "--message-format=json"]);
    assert_eq!(bare, wrapped);
    assert!(bare.stdout.contains("\"reason\":\"build-finished\""));
}

/// A test that announces itself with a file and then sleeps, so the harness
/// can signal `ck` at a moment when the test binary is known to be running.
fn sleeping_test(marker: &Path) -> String {
    format!(
        "#[cfg(test)]\nmod tests {{\n    #[test]\n    fn sleeps() {{\n        \
         std::fs::write({marker:?}, b\"\").unwrap();\n        \
         std::thread::sleep(std::time::Duration::from_secs(30));\n    }}\n}}\n"
    )
}

#[test]
fn an_interrupted_run_dumps_what_it_captured_and_reports_the_signal() {
    let _serial = SERIAL.lock().unwrap();
    let probe = Probe::with_lib(GREEN);
    let marker = probe.dir.join("running");
    probe.set_lib(&sleeping_test(&marker));

    let mut child = probe
        .command(CK, &["cargo", "test"])
        .spawn()
        .expect("spawn ck");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "test binary never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: child.id() is a live child of this process.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };

    let status = child.wait().expect("wait");
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert_eq!(status.code(), Some(128 + libc::SIGTERM));
    assert!(
        stdout.contains("running 1 test"),
        "captured output before the interrupt should be dumped, got: {stdout:?}"
    );
    assert!(
        probe.baselines().is_empty(),
        "an interrupted run writes nothing"
    );
}

/// The three parts of the shadow-mode view: the normal output, the parse,
/// and where the raw output begins.
fn verify(probe: &Probe, lib_rs: &str, cargo_args: &[&str]) -> (String, String, Option<i32>) {
    let _serial = SERIAL.lock().unwrap();
    probe.set_lib(lib_rs);
    let mut args = vec!["--verify", "cargo"];
    args.extend_from_slice(cargo_args);
    let out = probe.run(CK, &args);
    let (head, rest) = out
        .stdout
        .split_once("---- parsed ----\n")
        .expect("verify shows the parse");
    let (parsed, _raw) = rest
        .split_once("---- raw output ----\n")
        .expect("verify ends with the raw output");
    (head.to_string(), parsed.to_string(), out.code)
}

#[test]
fn verify_shows_the_normal_output_then_the_parse_then_the_raw() {
    let probe = Probe::with_lib(BROKEN);
    let (head, parsed, code) = verify(&probe, BROKEN, &["build"]);
    assert_eq!(code, Some(101));
    assert!(
        head.starts_with("ck: baseline recorded (1 error, 0 warnings)"),
        "{head}"
    );
    assert!(
        parsed.starts_with("1 error, 0 warnings, exit 101\n"),
        "{parsed}"
    );
    assert!(parsed.contains("error[E0425] src/lib.rs:1:26  cannot find function `missing`"));
    assert!(parsed.contains("src/lib.rs|E0425|missing  [constructed]"));
    // The raw output still reaches stderr, where cargo puts diagnostics.
    let out = probe.run(CK, &["--verify", "cargo", "build"]);
    assert!(out.stderr.contains("cannot find function `missing`"));

    // Shadow mode is the real run: the baseline advanced, and the second
    // view carries the comparison.
    let (head, parsed, code) = verify(&probe, BROKEN, &["build"]);
    assert_eq!(code, Some(0));
    assert!(
        head.starts_with("ck: 1 error, 0 warnings, nothing new\n"),
        "{head}"
    );
    assert!(head.contains("STILL FAILING (1)"));
    assert!(
        parsed.starts_with("1 error, 0 warnings, exit 101\n"),
        "{parsed}"
    );
}

#[test]
fn verify_reports_a_failed_command_that_produced_no_diagnostics() {
    let probe = Probe::with_lib(FAILING_TEST);
    let (head, parsed, code) = verify(&probe, FAILING_TEST, &["test"]);
    assert_eq!(code, Some(101));
    assert_eq!(head, "", "an unparsed run has no verdict");
    assert!(parsed.contains("0 errors, 0 warnings, exit 101"));
    assert!(parsed.contains("output after the build was not parsed"));
    assert!(parsed.contains("exited 101: not a clean run"));
    assert!(probe.baselines().is_empty());
}

#[test]
fn verify_on_a_clean_build_says_so_and_exits_zero() {
    let probe = Probe::with_lib(GREEN);
    let (head, parsed, code) = verify(&probe, GREEN, &["check"]);
    assert_eq!(code, Some(0));
    assert!(head.starts_with("ck: baseline recorded (0 errors, 0 warnings)"));
    assert_eq!(parsed, "0 errors, 0 warnings, exit 0\n");
}

// The fix loop: the reason the tool exists.

#[test]
fn the_same_failure_again_is_quiet_and_does_not_gate() {
    let probe = Probe::with_lib(BROKEN);
    let _serial = SERIAL.lock().unwrap();
    probe.wrapped(BROKEN, &["build"]);
    let again = probe.wrapped(BROKEN, &["build"]);
    assert_eq!(again.code, Some(0), "nothing new: the gate is open");
    assert_eq!(
        again.stderr, "",
        "the runner's output is replaced by the verdict"
    );
    let lines: Vec<&str> = again.stdout.lines().collect();
    assert_eq!(lines[0], "ck: 1 error, 0 warnings, nothing new");
    assert_eq!(lines[1], "");
    assert_eq!(lines[2], "STILL FAILING (1)");
    assert!(
        lines[3].contains(
            "  error[E0425] src/lib.rs:1:26  cannot find function `missing` in this scope  (2 runs)"
        ),
        "{}",
        lines[3]
    );
    assert_eq!(lines[5], "  detail: ck show <id>");
    assert_eq!(lines.len(), 6);
}

#[test]
fn a_fix_then_a_regression_then_a_second_failure() {
    let probe = Probe::with_lib(BROKEN);
    let _serial = SERIAL.lock().unwrap();
    probe.wrapped(BROKEN, &["build"]);

    let fixed = probe.wrapped(GREEN, &["build"]);
    assert_eq!(fixed.code, Some(0));
    assert_eq!(
        fixed.stdout,
        "ck: 0 errors, 0 warnings, 0 new, 1 fixed\n\nFIXED (1)\n"
    );

    let quiet = probe.wrapped(GREEN, &["build"]);
    assert_eq!(quiet.stdout, "ck: 0 errors, 0 warnings, nothing new\n");
    assert_eq!(quiet.code, Some(0));

    let regressed = probe.wrapped(BROKEN, &["build"]);
    assert_eq!(
        regressed.code,
        Some(1),
        "a new error on a failed build gates"
    );
    assert!(
        regressed
            .stdout
            .starts_with("ck: 1 error, 0 warnings, 1 new, 0 fixed\n\nNEW (1)\n")
    );
    assert!(
        regressed
            .stdout
            .contains("  error[E0425]: cannot find function `missing` in this scope\n"),
        "new failures get the runner's full detail: {}",
        regressed.stdout
    );
    assert!(regressed.stdout.contains("  --> src/lib.rs:1:26\n"));
    assert!(!regressed.stdout.contains("STILL FAILING"));

    let two = "pub fn answer() -> u32 { missing() + absent() }\n";
    let second = probe.wrapped(two, &["build"]);
    assert_eq!(second.code, Some(1));
    assert!(
        second
            .stdout
            .starts_with("ck: 2 errors, 0 warnings, 1 new, 0 fixed\n")
    );
    assert!(second.stdout.contains("NEW (1)\n"));
    assert!(second.stdout.contains("cannot find function `absent`"));
    assert!(second.stdout.contains("STILL FAILING (1)\n"));
    assert!(
        second
            .stdout
            .contains("cannot find function `missing` in this scope  (2 runs)")
    );
}

#[test]
fn a_new_warning_is_reported_and_never_gates() {
    let probe = Probe::with_lib(GREEN);
    let _serial = SERIAL.lock().unwrap();
    probe.wrapped(GREEN, &["check"]);
    let warned = probe.wrapped(WARNING, &["check"]);
    assert_eq!(warned.code, Some(0));
    assert!(
        warned
            .stdout
            .starts_with("ck: 0 errors, 1 warning, 1 new, 0 fixed\n\nNEW (1)\n"),
        "{}",
        warned.stdout
    );
    assert!(warned.stdout.contains("unused variable: `unused`"));
}

#[test]
fn branches_and_commands_keep_separate_baselines() {
    let probe = Probe::with_lib(BROKEN);
    let _serial = SERIAL.lock().unwrap();
    probe.on_branch("main");
    probe.wrapped(BROKEN, &["build"]);
    assert!(
        probe
            .wrapped(BROKEN, &["build"])
            .stdout
            .contains("nothing new")
    );

    // Another command on the same branch is a different question.
    let check = probe.wrapped(BROKEN, &["check"]);
    assert!(
        check.stdout.contains("ck: baseline recorded"),
        "{}",
        check.stdout
    );

    // The same command on another branch starts over.
    probe.on_branch("experiment");
    let elsewhere = probe.wrapped(BROKEN, &["build"]);
    assert!(
        elsewhere.stdout.contains("ck: baseline recorded"),
        "{}",
        elsewhere.stdout
    );
    assert_eq!(elsewhere.code, Some(101));

    // And coming back finds the original.
    probe.on_branch("main");
    let back = probe.wrapped(BROKEN, &["build"]);
    assert!(back.stdout.contains("nothing new"), "{}", back.stdout);
    assert_eq!(probe.baselines().len(), 3);
}

#[test]
fn a_failure_the_parse_did_not_see_dumps_raw_and_leaves_the_baseline_alone() {
    // A build script that panics fails the build without a single compiler
    // diagnostic: the exit code says failure and the parse has nothing.
    let probe = Probe::with_lib(GREEN);
    let _serial = SERIAL.lock().unwrap();
    probe.wrapped(GREEN, &["build"]);
    fs::write(
        probe.dir.join("build.rs"),
        "fn main() { panic!(\"no build for you\"); }\n",
    )
    .unwrap();
    let out = probe.wrapped(GREEN, &["build"]);
    assert_eq!(out.code, Some(101));
    assert!(out.stderr.contains("no build for you"), "{}", out.stderr);
    assert!(
        out.stderr
            .contains("ck: the command exited 101 but no error was found")
    );
    assert!(!out.stdout.contains("ck:"), "no verdict: {}", out.stdout);
    fs::remove_file(probe.dir.join("build.rs")).unwrap();

    // The baseline is the green one still.
    let after = probe.wrapped(GREEN, &["build"]);
    assert_eq!(after.stdout, "ck: 0 errors, 0 warnings, nothing new\n");
}

#[test]
fn a_baseline_ck_cannot_read_is_reported_and_replaced() {
    let probe = Probe::with_lib(BROKEN);
    let _serial = SERIAL.lock().unwrap();
    probe.wrapped(BROKEN, &["build"]);
    let baseline = probe.baselines().pop().unwrap();
    fs::write(&baseline, "{\"schema\": 999}").unwrap();
    let out = probe.wrapped(BROKEN, &["build"]);
    assert!(out.stderr.contains("schema 999"), "{}", out.stderr);
    assert!(out.stderr.contains("starting over"));
    assert!(out.stdout.contains("ck: baseline recorded"));
    assert!(
        probe
            .wrapped(BROKEN, &["build"])
            .stdout
            .contains("nothing new")
    );
}

#[test]
fn without_anywhere_to_keep_a_baseline_every_run_is_first_contact() {
    let probe = Probe::with_lib(BROKEN);
    let _serial = SERIAL.lock().unwrap();
    fs::write(probe.dir.join("cache"), "").unwrap();
    for _ in 0..2 {
        let out = probe.wrapped(BROKEN, &["build"]);
        assert_eq!(out.code, Some(101));
        assert!(out.stdout.contains("ck: baseline recorded"));
        assert!(
            out.stderr.contains("nowhere to keep a baseline"),
            "{}",
            out.stderr
        );
    }
}

// Reading the store back.

/// The handle on the first STILL FAILING line.
fn first_handle(stdout: &str) -> String {
    let line = stdout
        .lines()
        .skip_while(|l| !l.starts_with("STILL FAILING"))
        .nth(1)
        .expect("a still-failing line");
    line.split_whitespace().next().unwrap().to_string()
}

#[test]
fn show_prints_the_stored_detail_for_a_handle() {
    let probe = Probe::with_lib(BROKEN);
    let _serial = SERIAL.lock().unwrap();
    probe.wrapped(BROKEN, &["build"]);
    let handle = first_handle(&probe.wrapped(BROKEN, &["build"]).stdout);
    assert_eq!(handle.len(), 10);

    let shown = probe.run(CK, &["show", &handle]);
    assert_eq!(shown.code, Some(0));
    assert!(
        shown
            .stdout
            .starts_with("error[E0425]: cannot find function `missing` in this scope\n"),
        "{}",
        shown.stdout
    );
    assert!(shown.stdout.contains(" --> src/lib.rs:1:26\n"));
    assert!(
        shown.stdout.ends_with(&format!(
            "\nck: {handle} from `cargo build`: failing for 2 runs, last run just now\n"
        )),
        "{}",
        shown.stdout
    );

    // Fixed, the failure is still there to be shown, and says so.
    probe.wrapped(GREEN, &["build"]);
    let shown = probe.run(CK, &["show", &handle]);
    assert!(
        shown.stdout.contains("not seen for 1 run"),
        "{}",
        shown.stdout
    );
}

#[test]
fn show_of_an_unknown_handle_says_so_and_exits_two() {
    let probe = Probe::with_lib(GREEN);
    let out = probe.run(CK, &["show", "0000000000"]);
    assert_eq!(out.code, Some(2));
    assert_eq!(out.stdout, "");
    assert!(out.stderr.starts_with("ck: no failure with id 0000000000"));
}

#[test]
fn bare_ck_lists_the_baselines_for_this_tree_and_branch() {
    let probe = Probe::with_lib(BROKEN);
    let _serial = SERIAL.lock().unwrap();
    probe.on_branch("main");

    let empty = probe.run(CK, &[]);
    assert_eq!(empty.code, Some(0));
    assert!(
        empty.stdout.starts_with("ck: no baselines on main;"),
        "{}",
        empty.stdout
    );

    probe.wrapped(BROKEN, &["build"]);
    probe.wrapped(BROKEN, &["build"]);
    probe.wrapped(WARNING, &["check"]);
    let listed = probe.run(CK, &[]);
    assert_eq!(listed.code, Some(0));
    let lines: Vec<&str> = listed.stdout.lines().collect();
    assert_eq!(lines[0], "ck: 2 baselines on main");
    assert_eq!(
        lines[1],
        "  cargo build   1 error, 0 warnings   2 runs, just now"
    );
    assert_eq!(
        lines[2],
        "  cargo check   0 errors, 1 warning   1 run, just now"
    );
    assert_eq!(lines.len(), 3);

    probe.on_branch("experiment");
    assert!(
        probe
            .run(CK, &[])
            .stdout
            .starts_with("ck: no baselines on experiment;")
    );
}
