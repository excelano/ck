//! The normalized record every adapter produces.
//!
//! This is the one type that is expensive to retrofit, so it is shaped for
//! the whole design rather than for the adapters that exist today: a
//! failure carries the tier and confidence of its own identity, detail is
//! captured at ingest rather than reconstructed later, and the raw output is
//! always retained. Nothing in here knows what a runner is.

use serde::{Deserialize, Serialize};

use crate::adapter::AdapterId;
use crate::exec::Captured;

/// Everything one captured run amounts to.
#[derive(Debug)]
pub struct RunReport {
    pub adapter: AdapterId,
    /// How far the parse can be trusted. A report is only ever compared on
    /// `Clean`; the other two fall back to the raw output.
    pub parse: Parse,
    /// Counts the runner itself reported. `None` when it did not say; a
    /// derived total would look plausible and be unverifiable.
    pub totals: Option<Totals>,
    pub failures: Vec<Failure>,
    /// Kept even on a fully successful parse: shadow mode prints it, and any
    /// later disagreement between the parse and the exit code needs it.
    pub raw: Captured,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parse {
    /// Everything the runner produced was understood.
    Clean,
    /// The part ck understands was parsed, and something followed it that
    /// was not: output from a program the build then ran, for instance.
    Partial,
    /// The output was not what the adapter expected. The reason is for the
    /// caller; the raw output is the answer.
    Failed(String),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
pub struct Totals {
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub identity: Identity,
    pub severity: Severity,
    /// A lint name or an error code, when the runner gave one.
    pub rule: Option<String>,
    /// Payload, never identity. Lines move.
    pub location: Option<Location>,
    /// One line.
    pub message: String,
    /// The runner's full text for this failure, for `show`.
    pub detail: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct Location {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub tier: Tier,
    pub confidence: Confidence,
    /// What comparison matches on. Readable, since shadow mode shows it, but
    /// not something to type: see `handle`.
    pub key: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// The runner's own name for the thing.
    Node,
    /// Built from file, rule, and a discriminator lifted from the message.
    Constructed,
    /// The normalized message itself. Last resort.
    Hash,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Canonical,
    /// Worth a marker when ck claims something is new on it.
    Low,
}

impl Identity {
    /// A short, shell-safe name for the key, for `ck show`. Keys carry
    /// whatever the runner's message did, including characters a shell
    /// would interpret; the handle is what gets typed back.
    pub fn handle(&self) -> String {
        format!("{:010x}", fnv1a(self.key.as_bytes()) & 0xff_ffff_ffff)
    }
}

/// FNV-1a, 64-bit. Written out rather than taken from the standard hasher
/// because handles and store paths are compared across runs, and the
/// standard hasher does not promise the same output from one toolchain to
/// the next.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_stable_short_and_hex() {
        let id = Identity {
            tier: Tier::Constructed,
            confidence: Confidence::Canonical,
            key: "src/stats.rs|E0425|frequencies".into(),
        };
        assert_eq!(id.handle(), id.handle());
        assert_eq!(id.handle().len(), 10);
        assert!(id.handle().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn different_keys_get_different_handles() {
        let a = Identity {
            tier: Tier::Hash,
            confidence: Confidence::Low,
            key: "a".into(),
        };
        let b = Identity {
            key: "b".into(),
            ..a.clone()
        };
        assert_ne!(a.handle(), b.handle());
    }
}
