//! Running the wrapped command.
//!
//! Passthrough uses inherited stdio rather than pipes. The child writes
//! straight to the real descriptors, so it keeps TTY detection, colour,
//! progress rendering, and the true interleaving of stdout and stderr. Only
//! the adapter path needs interception, and it pays for that separately;
//! there is no reason to make every unrecognised command pay it too.
//!
//! Captured mode is the same spawn with pipes on stdout and stderr, each
//! drained by its own thread so that neither can fill and stall the child.
//! Chunks are kept in arrival order across both streams rather than as two
//! separate buffers, because the order is most of what makes a build log
//! readable when it is replayed. No PTY: a PTY would merge the two streams,
//! and the split is worth more than the child's colour.
//!
//! The child runs in its own process group so that an interrupt can reach the
//! whole tree — test runners spawn children of their own, and the failure being
//! avoided is an orphan holding a port after a cancelled run. That choice costs
//! something back: a background process group reading from the terminal is
//! stopped with SIGTTIN. So when stdin is a terminal, the child's group is made
//! the foreground group for the duration and handed back afterwards, which is
//! what a shell does and what makes Ctrl-C reach the child directly.
//!
//! One consequence looks like a bug and is not. POSIX requires a non-interactive
//! shell to set SIGINT and SIGQUIT to SIG_IGN for the jobs it backgrounds, so
//! `ck sh -c 'worker & main'` leaves the backgrounded worker running after an
//! interrupt. Verified against the bare command, which does the same thing:
//! SIGINT leaves the background job, SIGTERM takes it. Matching the raw command
//! is the promise; do not "fix" this by escalating SIGINT to SIGTERM.

use std::io::{IsTerminal, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long a signalled process group has to exit on its own before it is
/// killed outright.
const GRACE: Duration = Duration::from_secs(5);

/// How long, after the child has exited, to keep reading its pipes. The
/// pipes stay open while anything the child started still holds them, and a
/// daemon left behind by a test would otherwise hold `ck` open with it. The
/// bare command returns in that situation; so must this.
const LINGER: Duration = Duration::from_millis(500);

/// Which of the child's two output streams a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One read from one of the child's pipes.
#[derive(Debug)]
pub struct Chunk {
    pub stream: Stream,
    pub bytes: Vec<u8>,
}

/// Everything a captured run produced, in the order it arrived.
#[derive(Debug)]
pub struct Captured {
    pub chunks: Vec<Chunk>,
    pub status: ExitStatus,
}

enum Event {
    Chunk(Chunk),
    Closed,
}

/// The wrapped child's process group, for the signal handler to reach. Zero
/// until the child is spawned.
static CHILD_PGID: AtomicI32 = AtomicI32::new(0);
/// Set once the child has been reaped, so the watchdog does not signal a pid
/// that has since been recycled.
static REAPED: AtomicBool = AtomicBool::new(false);
/// Write end of the self-pipe the handler nudges.
static PIPE_W: AtomicI32 = AtomicI32::new(-1);

/// Signal handler. Everything here must be async-signal-safe, which is why it
/// does nothing but write one byte; the forwarding and the timing happen on an
/// ordinary thread reading the other end.
extern "C" fn nudge(sig: libc::c_int) {
    let fd = PIPE_W.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = sig as u8;
        unsafe {
            libc::write(fd, std::ptr::from_ref(&byte).cast(), 1);
        }
    }
}

/// Run `argv` with `env` added to its environment, streams inherited, and
/// return the exit code `ck` itself should exit with.
pub fn run(env: &[(String, String)], argv: &[String]) -> i32 {
    match launch(env, argv, false) {
        Ok(captured) => exit_code(&captured.status),
        Err(code) => code,
    }
}

/// Run `argv` with its output captured. `Err` carries an exit code for a
/// command that never started or could not be waited for; the caller has
/// already been told why.
pub fn capture(env: &[(String, String)], argv: &[String]) -> Result<Captured, i32> {
    launch(env, argv, true)
}

/// The exit code `ck` reports for a finished child.
pub fn exit_code(status: &ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        // The shell convention, and the only honest answer: the run produced
        // no verdict because it did not finish.
        128 + sig
    } else {
        // Neither an exit code nor a signal is not a documented possibility;
        // refusing to invent a verdict is the whole point.
        eprintln!("ck: the command ended in a way ck could not interpret");
        2
    }
}

fn launch(env: &[(String, String)], argv: &[String], capture: bool) -> Result<Captured, i32> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for (name, value) in env {
        cmd.env(name, value);
    }
    // 0 means "your own pid": the child becomes its own group leader.
    cmd.process_group(0);
    if capture {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    }

    // Handlers go in before the child exists, not after it. Between spawn
    // returning and a handler being installed, the default disposition applies:
    // a SIGTERM in that window kills ck outright and orphans the whole group,
    // which is the exact failure the group machinery exists to prevent. The
    // window is microseconds and it is still real. A signal that arrives before
    // the child is spawned leaves its byte in the pipe, and the supervisor —
    // started afterwards — consumes it against a process group that exists by
    // then.
    let listener = install_handlers();

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(spawn_failure(&argv[0], &e)),
    };

    let pgid = child.id() as i32;
    // The parent sets the group too, which is the standard way to close the
    // race between spawn returning and the child reaching its own setpgid.
    // In that window the child is still in ck's group, so kill(-pgid) finds no
    // such group, returns ESRCH, and the interrupt is silently lost — the
    // symptom is an interrupt that occasionally does nothing and a command
    // that runs until the grace period kills it. Whichever of the two calls
    // lands first wins; the loser fails harmlessly.
    // SAFETY: pgid is this process's live child. EACCES once it has exec'd,
    // and EPERM if it already set its own group, are both expected here.
    unsafe { libc::setpgid(pgid, pgid) };
    CHILD_PGID.store(pgid, Ordering::SeqCst);

    let terminal = Terminal::hand_over(pgid);
    if let Some(read_fd) = listener {
        std::thread::spawn(move || supervise(read_fd));
    }

    // The readers start before the wait, or a child that fills a pipe blocks
    // forever with nobody on the other end.
    let (events, readers) = mpsc::channel();
    let mut open = 0;
    if let Some(out) = child.stdout.take() {
        drain(Stream::Stdout, out, events.clone());
        open += 1;
    }
    if let Some(err) = child.stderr.take() {
        drain(Stream::Stderr, err, events.clone());
        open += 1;
    }
    drop(events);

    let status = child.wait();
    REAPED.store(true, Ordering::SeqCst);
    terminal.take_back();

    let mut chunks = Vec::new();
    let deadline = Instant::now() + LINGER;
    while open > 0 {
        let wait = deadline.saturating_duration_since(Instant::now());
        match readers.recv_timeout(wait) {
            Ok(Event::Chunk(chunk)) => chunks.push(chunk),
            Ok(Event::Closed) => open -= 1,
            Err(_) => break,
        }
    }

    match status {
        Ok(status) => Ok(Captured { chunks, status }),
        Err(e) => {
            eprintln!("ck: could not wait for the command: {e}");
            Err(2)
        }
    }
}

/// Read one of the child's pipes to the end on its own thread, handing every
/// chunk to the channel as it arrives.
fn drain(stream: Stream, mut source: impl Read + Send + 'static, events: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match source.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = Chunk {
                        stream,
                        bytes: buf[..n].to_vec(),
                    };
                    if events.send(Event::Chunk(chunk)).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = events.send(Event::Closed);
    });
}

/// Words a shell runs itself. There is no executable behind them, so no
/// wrapper can run them — `cd` is the one that matters most, because a child
/// process cannot change its parent's directory even in principle.
///
/// Checked only after an exec has already failed. A name on this list that
/// does exist as a program on PATH still runs normally, which is why the check
/// belongs here and not in front of the spawn.
const SHELL_WORDS: &[&str] = &[
    ".",
    "alias",
    "bg",
    "bind",
    "builtin",
    "caller",
    "case",
    "cd",
    "command",
    "compgen",
    "complete",
    "declare",
    "dirs",
    "disown",
    "do",
    "done",
    "elif",
    "else",
    "enable",
    "esac",
    "eval",
    "exec",
    "exit",
    "export",
    "fg",
    "fi",
    "for",
    "function",
    "getopts",
    "hash",
    "history",
    "if",
    "in",
    "jobs",
    "let",
    "local",
    "logout",
    "mapfile",
    "popd",
    "pushd",
    "read",
    "readarray",
    "readonly",
    "return",
    "select",
    "set",
    "shift",
    "shopt",
    "source",
    "suspend",
    "then",
    "times",
    "trap",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "unset",
    "until",
    "while",
];

/// Report a command that never started, with the conventional code. This is
/// never a clean run and never a comparison.
fn spawn_failure(program: &str, e: &std::io::Error) -> i32 {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound if SHELL_WORDS.contains(&program) => {
            // Principle 7: the error is a return value. "command not found"
            // here is true and useless — it sends the caller looking for a
            // missing binary rather than telling them the thing they wrote can
            // never be wrapped.
            eprintln!(
                "ck: {program} is a shell builtin, not a program.\n\
                 \n\
                 There is no executable behind it, so ck has nothing to run. Use it\n\
                 without ck:\n\
                 \n    {program} ..."
            );
            127
        }
        ErrorKind::NotFound => {
            eprintln!("ck: {program}: command not found");
            127
        }
        ErrorKind::PermissionDenied => {
            eprintln!("ck: {program}: permission denied");
            126
        }
        _ => {
            eprintln!("ck: {program}: could not run: {e}");
            126
        }
    }
}

/// Install the handlers that forward an interrupt to the child's group, and
/// return the read end the supervisor should watch. `None` means forwarding is
/// unavailable and the caller has already been told.
fn install_handlers() -> Option<libc::c_int> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: fds is a valid two-element array for the duration of the call.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        // Without the pipe there is no safe way to forward. The child still
        // runs; an interrupt just will not reach it. Say so rather than
        // pretending the guarantee holds.
        eprintln!("ck: could not set up signal forwarding; interrupts will not reach the command");
        return None;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    for fd in [read_fd, write_fd] {
        // SAFETY: fd is a pipe end this function owns.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    PIPE_W.store(write_fd, Ordering::SeqCst);

    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: `nudge` is async-signal-safe.
        unsafe { libc::signal(sig, nudge as *const () as libc::sighandler_t) };
    }

    Some(read_fd)
}

/// Forward the first signal to the child's group, then give the group a grace
/// period before killing it outright. Group-level matters because the runner's
/// own children are what get orphaned otherwise.
fn supervise(read_fd: libc::c_int) {
    let mut byte = 0u8;
    // SAFETY: read_fd is the read end of the pipe this thread owns.
    let n = unsafe { libc::read(read_fd, std::ptr::from_mut(&mut byte).cast(), 1) };
    if n != 1 {
        return;
    }
    // A signal can arrive before the child's group id has been recorded. The
    // byte is already in the pipe by then, so the only thing to do is wait for
    // the group rather than drop the interrupt on the floor.
    let mut pgid = CHILD_PGID.load(Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while pgid <= 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
        pgid = CHILD_PGID.load(Ordering::SeqCst);
    }
    if pgid <= 0 {
        return;
    }
    // SAFETY: signalling a group that has already exited returns ESRCH.
    unsafe { libc::kill(-pgid, libc::c_int::from(byte)) };

    std::thread::sleep(GRACE);
    if !REAPED.load(Ordering::SeqCst) {
        // SAFETY: as above.
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
    }
}

/// The controlling terminal, lent to the child's process group and taken back
/// afterwards. A no-op when stdin is not a terminal, which is the agent case.
struct Terminal {
    restore_to: Option<libc::pid_t>,
}

impl Terminal {
    fn hand_over(pgid: i32) -> Self {
        if !std::io::stdin().is_terminal() {
            return Self { restore_to: None };
        }
        // SAFETY: fd 0 is a terminal, checked above.
        let previous = unsafe { libc::tcgetpgrp(0) };
        if previous < 0 {
            return Self { restore_to: None };
        }
        // tcsetpgrp from a background group raises SIGTTOU at the caller, which
        // would stop ck. Ignoring it across the two calls is the standard
        // shell dance.
        // SAFETY: SIG_IGN is a valid disposition for SIGTTOU.
        unsafe {
            libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::tcsetpgrp(0, pgid);
        }
        Self {
            restore_to: Some(previous),
        }
    }

    fn take_back(self) {
        if let Some(previous) = self.restore_to {
            // SAFETY: `previous` came from tcgetpgrp on this same descriptor.
            unsafe {
                libc::tcsetpgrp(0, previous);
                libc::signal(libc::SIGTTOU, libc::SIG_DFL);
            }
        }
    }
}
