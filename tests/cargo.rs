//! The cargo path, tested the same way as passthrough: run a build bare and
//! through `ck`, and require the two to read the same. Cargo is switched into
//! its JSON mode underneath, so this is the test that the replay reproduces
//! cargo's own output rather than something that merely resembles it.
//!
//! Each test builds a throwaway crate of its own. Runs are serialised because
//! cargo's package-cache lock announces itself on stderr when contended, and
//! that line would appear in one run and not the other.

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
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn run(&self, program: &str, args: &[&str]) -> Outcome {
        let out = self.command(program, args).output().expect("run");
        Outcome {
            stdout: normalise(&out.stdout),
            stderr: normalise(&out.stderr),
            code: out.status.code(),
        }
    }

    /// The same cargo invocation bare and through `ck`, both from a fresh
    /// mtime so both actually compile.
    fn both(&self, lib_rs: &str, cargo_args: &[&str]) -> (Outcome, Outcome) {
        let _serial = SERIAL.lock().unwrap();
        self.set_lib(lib_rs);
        let bare = self.run("cargo", cargo_args);
        self.set_lib(lib_rs);
        let mut ck_args = vec!["cargo"];
        ck_args.extend_from_slice(cargo_args);
        let wrapped = self.run(CK, &ck_args);
        (bare, wrapped)
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

/// Remove what legitimately differs between two runs of the same build.
///
/// Elapsed times (`0.42s`) and libtest's thread ids (`(110692)`) change every
/// run. The per-crate summary that cargo prints in human mode only —
/// `warning: \`probe\` (lib) generated 1 warning` — is a line cargo composes
/// itself and does not emit in JSON mode; the diagnostics it counts are
/// reproduced in full, and the count is what `ck`'s own verdict will carry.
fn normalise(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
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
fn a_green_build_reads_the_same() {
    let probe = Probe::with_lib(GREEN);
    let (bare, wrapped) = probe.both(GREEN, &["build"]);
    assert_eq!(bare, wrapped);
    assert_eq!(bare.code, Some(0));
}

#[test]
fn a_broken_build_reads_the_same_and_fails_the_same() {
    let probe = Probe::with_lib(BROKEN);
    let (bare, wrapped) = probe.both(BROKEN, &["build"]);
    assert_eq!(bare, wrapped);
    assert_eq!(bare.code, Some(101));
    assert!(bare.stderr.contains("cannot find function `missing`"));
}

#[test]
fn warnings_read_the_same_and_still_succeed() {
    let probe = Probe::with_lib(WARNING);
    let (bare, wrapped) = probe.both(WARNING, &["check"]);
    assert_eq!(bare, wrapped);
    assert_eq!(bare.code, Some(0));
    assert!(bare.stderr.contains("unused variable"));
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
}

/// The whole verify view on stdout, cut at the raw-output separator.
fn verdict(probe: &Probe, lib_rs: &str, cargo_args: &[&str]) -> (String, Option<i32>) {
    let _serial = SERIAL.lock().unwrap();
    probe.set_lib(lib_rs);
    let mut args = vec!["--verify", "cargo"];
    args.extend_from_slice(cargo_args);
    let out = probe.run(CK, &args);
    let head = out
        .stdout
        .split("---- raw output ----\n")
        .next()
        .unwrap()
        .to_string();
    assert!(
        out.stdout.contains("---- raw output ----\n"),
        "verify should end with the raw output"
    );
    (head, out.code)
}

#[test]
fn verify_names_each_diagnostic_and_then_shows_the_raw_output() {
    let probe = Probe::with_lib(BROKEN);
    let (head, code) = verdict(&probe, BROKEN, &["build"]);
    assert_eq!(code, Some(101));
    assert!(
        head.starts_with("ck --verify: 1 error, 0 warnings, exit 101\n"),
        "{head}"
    );
    assert!(head.contains("error[E0425] src/lib.rs:1:26  cannot find function `missing`"));
    assert!(head.contains("src/lib.rs|E0425|missing  [constructed]"));
    // The raw output still reaches stderr, where cargo puts diagnostics.
    let out = probe.run(CK, &["--verify", "cargo", "build"]);
    assert!(out.stderr.contains("cannot find function `missing`"));
}

#[test]
fn verify_reports_a_failed_command_that_produced_no_diagnostics() {
    let probe = Probe::with_lib(FAILING_TEST);
    let (head, code) = verdict(&probe, FAILING_TEST, &["test"]);
    assert_eq!(code, Some(101));
    assert!(head.contains("0 errors, 0 warnings, exit 101"));
    assert!(head.contains("output after the build was not parsed"));
    assert!(head.contains("exited 101: not a clean run"));
}

#[test]
fn verify_on_a_clean_build_says_so_and_exits_zero() {
    let probe = Probe::with_lib(GREEN);
    let (head, code) = verdict(&probe, GREEN, &["check"]);
    assert_eq!(code, Some(0));
    assert_eq!(head, "ck --verify: 0 errors, 0 warnings, exit 0\n");
}
