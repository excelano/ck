//! Runners ck understands.
//!
//! Matching is decided on the command line before the child is spawned, not
//! on its output afterwards, because the decision changes how the child is
//! run: a matched runner is switched into its machine-readable mode and has
//! its streams captured, an unmatched one inherits the terminal and is never
//! touched. Nothing about any particular runner may leak past this module;
//! the rest of ck sees a rewritten command line and, later, a normalized
//! report.

pub mod cargo;

/// Which runner matched.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AdapterId {
    Cargo,
}

/// A recognized runner and the command line to actually run: the caller's,
/// with the runner's machine-readable mode switched on.
#[derive(Debug, PartialEq, Eq)]
pub struct Match {
    pub adapter: AdapterId,
    pub argv: Vec<String>,
}

/// Decide whether `argv` runs a runner ck understands.
pub fn detect(argv: &[String]) -> Option<Match> {
    cargo::detect(argv).map(|argv| Match {
        adapter: AdapterId::Cargo,
        argv,
    })
}
