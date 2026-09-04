//! Emitting the raw output, and capping it.
//!
//! The raw dump is the common path today, so it cannot be unbounded: a
//! build with two hundred diagnostics would deliver, through ck, the
//! same flood ck exists to prevent. When ck's own stdout is not a terminal
//! and the replay runs past the cap, the first `HEAD` and last `TAIL`
//! lines are printed with a marker between them, and the whole replay is
//! written to a file the marker names. A terminal gets everything; the
//! human is scrolling anyway, and the cap is for the caller whose context
//! window is the budget.
//!
//! The cut is counted in lines across both streams in arrival order, and
//! a line goes to the stream it came from, so what does get through still
//! reads as the runner's output. If the file cannot be written nothing is
//! cut at all: losing output silently is the one thing this must not do.

use std::io::{IsTerminal, Write};
use std::path::Path;

use crate::exec::Stream;

pub const HEAD: usize = 120;
pub const TAIL: usize = 40;

/// One line of the replay, newline included when the runner wrote one.
#[derive(Debug, PartialEq, Eq)]
pub struct Line {
    pub stream: Stream,
    pub bytes: Vec<u8>,
}

/// Print the replay, capped when it should be. `log` is where the full
/// replay goes if a cut is made; `None` means there is nowhere to put it,
/// and therefore no cut.
pub fn emit(lines: &[Line], log: Option<&Path>) {
    let capped = !std::io::stdout().is_terminal() && lines.len() > HEAD + TAIL;
    let saved = match (capped, log) {
        (true, Some(path)) => write_log(path, lines).is_ok(),
        _ => false,
    };

    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    // A write failure here means the reader has gone away, and there is
    // nobody left to tell.
    if saved {
        let hidden = lines.len() - HEAD - TAIL;
        write_all(&mut out, &mut err, &lines[..HEAD]);
        let _ = writeln!(
            out,
            "ck: {hidden} lines not shown ({} in all); full output in {}",
            lines.len(),
            log.unwrap_or(Path::new("")).display()
        );
        write_all(&mut out, &mut err, &lines[lines.len() - TAIL..]);
    } else {
        write_all(&mut out, &mut err, lines);
    }
    let _ = out.flush();
    let _ = err.flush();
}

fn write_all(out: &mut impl Write, err: &mut impl Write, lines: &[Line]) {
    for line in lines {
        let _ = match line.stream {
            Stream::Stdout => out.write_all(&line.bytes),
            Stream::Stderr => err.write_all(&line.bytes),
        };
    }
    // Interleaving is why both streams are flushed together: a stdout
    // line held in a buffer past the stderr line that followed it would
    // reorder what the runner printed.
    let _ = out.flush();
    let _ = err.flush();
}

/// The whole replay, both streams merged in order, as the runner's own
/// output would have read on a terminal. Temp-and-rename, like the
/// baseline, so a reader never sees half a file.
fn write_log(path: &Path, lines: &[Line]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let bytes: Vec<u8> = lines.iter().flat_map(|l| l.bytes.iter().copied()).collect();
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

/// Split chunks of one stream into lines, keeping the newline on each. A
/// final partial line is a line too.
pub fn split(stream: Stream, bytes: &[u8]) -> Vec<Line> {
    bytes
        .split_inclusive(|&b| b == b'\n')
        .map(|l| Line {
            stream,
            bytes: l.to_vec(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_keeps_newlines_and_a_trailing_fragment() {
        let lines = split(Stream::Stdout, b"one\ntwo\nthree");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].bytes, b"one\n");
        assert_eq!(lines[2].bytes, b"three");
        assert!(split(Stream::Stderr, b"").is_empty());
    }

    #[test]
    fn the_log_is_both_streams_in_order() {
        let dir = std::env::temp_dir().join(format!("ck-raw-{}", std::process::id()));
        let path = dir.join("deep/er/run.log");
        let lines = vec![
            Line {
                stream: Stream::Stderr,
                bytes: b"e1\n".to_vec(),
            },
            Line {
                stream: Stream::Stdout,
                bytes: b"o1\n".to_vec(),
            },
            Line {
                stream: Stream::Stderr,
                bytes: b"e2".to_vec(),
            },
        ];
        write_log(&path, &lines).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"e1\no1\ne2");
        let _ = std::fs::remove_dir_all(dir);
    }
}
