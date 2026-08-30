//! Running the wrapped command.
//!
//! Passthrough uses inherited stdio rather than pipes. The child writes
//! straight to the real descriptors, so it keeps TTY detection, colour,
//! progress rendering, and the true interleaving of stdout and stderr. Only
//! the adapter path needs interception, and it pays for that separately;
//! there is no reason to make every unrecognised command pay it too.
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

use std::io::IsTerminal;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

/// How long a signalled process group has to exit on its own before it is
/// killed outright.
const GRACE: Duration = Duration::from_secs(5);

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

/// Run `argv` and return the exit code `ck` itself should exit with.
pub fn run(argv: &[String]) -> i32 {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    // 0 means "your own pid": the child becomes its own group leader.
    cmd.process_group(0);

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
        Err(e) => return spawn_failure(&argv[0], &e),
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

    let status = child.wait();
    REAPED.store(true, Ordering::SeqCst);
    terminal.take_back();

    match status {
        Ok(s) => {
            if let Some(code) = s.code() {
                code
            } else if let Some(sig) = s.signal() {
                // The shell convention, and the only honest answer: the run
                // produced no verdict because it did not finish.
                128 + sig
            } else {
                // Neither an exit code nor a signal is not a documented
                // possibility; refusing to invent a verdict is the whole point.
                eprintln!("ck: the command ended in a way ck could not interpret");
                2
            }
        }
        Err(e) => {
            eprintln!("ck: could not wait for the command: {e}");
            2
        }
    }
}

/// Report a command that never started, with the conventional code. This is
/// never a clean run and never a comparison.
fn spawn_failure(program: &str, e: &std::io::Error) -> i32 {
    use std::io::ErrorKind;
    match e.kind() {
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
