//! The baseline store: one file per command, per branch, per repository.
//!
//! ```text
//! <root>/<repo>/<branch>/<command>.json
//! ```
//!
//! The root is `$CK_CACHE_DIR`, else `$XDG_CACHE_HOME/ck`, else
//! `~/.cache/ck`. Every segment is derived, never configured: the repository
//! from the working tree's path, the branch from `HEAD`, the command from
//! the command line as typed. A cross-branch or cross-command comparison is
//! the silent collision principle 1 exists to prevent, so nothing is ever
//! shared between two keys, and a key that cannot be derived produces no
//! store rather than a shared one.
//!
//! Two sessions running the same command on the same branch is an ordinary
//! day here, not an edge case. A lock is taken on open and held until the
//! slot is dropped, across the whole read-compare-write window. When it is
//! contended the comparison still happens and the write is skipped: a missed
//! update costs one stale comparison, while a poisoned one costs trust.
//! Writes go to a sibling temp file and are renamed into place, so a kill
//! mid-write leaves the previous baseline intact.

pub mod repo;

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::adapter::AdapterId;
use crate::report::{Failure, Totals, fnv1a};
use repo::{Branch, Place};

/// The layout of the baseline file and the identity algorithm behind its
/// keys, as one number: a file written under a different one is not
/// compared against, loudly. Unrelated to the release version.
pub const SCHEMA: u32 = 1;

/// The name of the environment variable that relocates the whole store.
pub const ROOT_VAR: &str = "CK_CACHE_DIR";

/// Where the store lives for this process, or `None` when nothing in the
/// environment says: no override, no XDG directory, no home.
pub fn root() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    root_from(var(ROOT_VAR), var("XDG_CACHE_HOME"), var("HOME"))
}

fn root_from(
    override_dir: Option<std::ffi::OsString>,
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        Some(PathBuf::from(dir))
    } else if let Some(xdg) = xdg {
        Some(PathBuf::from(xdg).join("ck"))
    } else {
        home.map(|home| PathBuf::from(home).join(".cache").join("ck"))
    }
}

/// Which baseline a run reads and writes.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Key {
    pub place: Place,
    pub command: CommandLine,
}

/// The command as the caller wrote it, before any adapter rewrote it. Stored
/// in the file too, so a listing can say what each baseline is for.
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct CommandLine {
    pub env: Vec<(String, String)>,
    pub argv: Vec<String>,
    /// The directory the command ran in, relative to the repository root.
    pub dir: String,
}

impl Key {
    pub fn new(place: Place, env: &[(String, String)], argv: &[String]) -> Self {
        let dir = place.dir.to_string_lossy().into_owned();
        Self {
            place,
            command: CommandLine {
                env: env.to_vec(),
                argv: argv.to_vec(),
                dir,
            },
        }
    }

    /// The baseline's path below the store root.
    pub fn path(&self) -> PathBuf {
        PathBuf::from(self.repo_segment())
            .join(self.branch_segment())
            .join(format!("{}.json", self.command_segment()))
    }

    /// The working tree's name, made unique by its full path. Two clones of
    /// one repository are two trees with two states, and must not share.
    fn repo_segment(&self) -> String {
        let root = &self.place.root;
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".to_string());
        let hash = fnv1a(root.as_os_str().as_encoded_bytes());
        format!("{}-{:010x}", encode(&name), hash & 0xff_ffff_ffff)
    }

    /// `@` cannot survive `encode`, so the two states that are not branches
    /// can never collide with one that is.
    fn branch_segment(&self) -> String {
        match &self.place.branch {
            Branch::Named(name) => encode(name),
            Branch::Detached(commit) => format!("@{commit}"),
            Branch::NoGit => "@no-git".to_string(),
        }
    }

    /// A readable prefix for the human, a hash of everything for the key.
    fn command_segment(&self) -> String {
        let c = &self.command;
        let mut slug = c
            .argv
            .iter()
            .take(2)
            .map(|t| encode(t))
            .collect::<Vec<_>>()
            .join("-");
        slug.truncate(32);
        let mut bytes = Vec::new();
        for (name, value) in &c.env {
            bytes.extend_from_slice(name.as_bytes());
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        bytes.push(0);
        for token in &c.argv {
            bytes.extend_from_slice(token.as_bytes());
            bytes.push(0);
        }
        bytes.push(0);
        bytes.extend_from_slice(c.dir.as_bytes());
        format!("{slug}-{:016x}", fnv1a(&bytes))
    }
}

/// Make a string safe as one path component without losing anything: every
/// byte outside `[A-Za-z0-9._-]` becomes `%XX`. Reversible, so two names
/// that differ cannot encode the same.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What is on disk under a key, and a lock over it.
///
/// A slot with no path (no store root, or a directory that could not be
/// created) reads as missing and never writes. That is the "no valid state
/// is universally safe" rule made literal: the run degrades to first
/// contact and the command is unaffected.
#[derive(Debug)]
pub struct Slot {
    path: Option<PathBuf>,
    /// Held while the slot lives. `None` when the lock is contended or the
    /// slot has no path.
    lock: Option<File>,
}

/// What a read found.
#[derive(Debug)]
pub enum Load {
    Found(Baseline),
    /// First contact.
    Missing,
    /// Something is there and it cannot be used: a different schema, a
    /// truncated write, a file that is not ck's. Treated as first contact,
    /// and said so, since a silent overwrite would hide a real problem.
    Unusable(String),
}

impl Slot {
    /// Open the baseline for `key` under the store root from the
    /// environment.
    pub fn open(key: &Key) -> Self {
        match root() {
            Some(root) => Self::open_in(&root, key),
            None => Self::detached(),
        }
    }

    /// Open the baseline for `key` under an explicit root.
    pub fn open_in(root: &Path, key: &Key) -> Self {
        let path = root.join(key.path());
        let Some(parent) = path.parent() else {
            return Self::detached();
        };
        if fs::create_dir_all(parent).is_err() {
            return Self::detached();
        }
        let lock = take_lock(&path.with_extension("lock"));
        Self {
            path: Some(path),
            lock,
        }
    }

    /// A slot backed by nothing.
    pub fn detached() -> Self {
        Self {
            path: None,
            lock: None,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether `save` would do anything.
    pub fn writable(&self) -> bool {
        self.lock.is_some()
    }

    pub fn load(&self) -> Load {
        let Some(path) = &self.path else {
            return Load::Missing;
        };
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Load::Missing,
            Err(e) => return Load::Unusable(format!("could not read {}: {e}", path.display())),
        };
        // The schema is checked before the rest is decoded so that a file
        // from another version reports as that, not as a parse error on
        // whatever field changed.
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => return Load::Unusable(format!("{} is not valid JSON: {e}", path.display())),
        };
        match value.get("schema").and_then(serde_json::Value::as_u64) {
            Some(schema) if schema == u64::from(SCHEMA) => {}
            Some(schema) => {
                return Load::Unusable(format!(
                    "{} was written with schema {schema}; this ck reads schema {SCHEMA}",
                    path.display()
                ));
            }
            None => {
                return Load::Unusable(format!("{} has no schema field", path.display()));
            }
        }
        match serde_json::from_value(value) {
            Ok(baseline) => Load::Found(baseline),
            Err(e) => Load::Unusable(format!("{} could not be decoded: {e}", path.display())),
        }
    }

    /// Write the baseline, or skip it. `Err` is only ever the reason it was
    /// skipped, which the caller may mention; it is never fatal.
    pub fn save(&self, baseline: &Baseline) -> Result<(), Skipped> {
        let Some(path) = &self.path else {
            return Err(Skipped::NoStore);
        };
        if self.lock.is_none() {
            return Err(Skipped::Contended);
        }
        let bytes = serde_json::to_vec_pretty(baseline).map_err(|e| Skipped::Io(e.to_string()))?;
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let written = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, path));
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(Skipped::Io(e.to_string()));
        }
        Ok(())
    }
}

/// Why a baseline was not written.
#[derive(Debug, PartialEq, Eq)]
pub enum Skipped {
    /// Nowhere to write: no root, or the directory could not be made.
    NoStore,
    /// Another ck holds this baseline right now.
    Contended,
    Io(String),
}

/// Take the advisory lock, or report that someone else has it. A filesystem
/// that cannot lock at all returns the file unlocked rather than refusing:
/// on such a filesystem the write is unprotected but it still happens, and
/// a store that never writes would be a store that never works.
fn take_lock(path: &Path) -> Option<File> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(std::fs::TryLockError::WouldBlock) => None,
        Err(std::fs::TryLockError::Error(_)) => Some(file),
    }
}

/// Seconds since the epoch, for the record. Wall time is enough: nothing
/// is ordered by it.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One baseline file.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub schema: u32,
    /// The release that wrote it. For a human reading the file; nothing
    /// decides on it.
    pub ck: String,
    pub command: CommandLine,
    pub adapter: AdapterId,
    pub recorded_at: u64,
    /// How many completed runs this baseline has seen, this one included.
    pub runs: u64,
    /// The child's exit code on the recorded run.
    pub exit: i32,
    pub totals: Option<Totals>,
    pub failures: Vec<Stored>,
}

/// A failure as the store keeps it: the record as ingested, and what has
/// happened to it since. Detail lives here from the first sighting, which
/// is what lets `show` answer for a failure that has since stopped printing.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    #[serde(flatten)]
    pub failure: Failure,
    /// One character per run since first sighting, oldest first: `F` when
    /// the failure was present, `P` when it was not. Capped, so the file
    /// does not grow with the age of the project. Flake detection reads
    /// this later without the file changing shape.
    pub history: String,
}

impl Stored {
    /// The most runs a history remembers.
    pub const HISTORY: usize = 32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Confidence, Identity, Location, Severity, Tier};
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn scratch() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ck-store-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn place(branch: Branch) -> Place {
        Place {
            root: PathBuf::from("/work/thing"),
            branch,
            dir: PathBuf::new(),
        }
    }

    fn key(branch: Branch, argv: &[&str]) -> Key {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        Key::new(place(branch), &[], &argv)
    }

    fn baseline(key: &Key) -> Baseline {
        Baseline {
            schema: SCHEMA,
            ck: "test".into(),
            command: key.command.clone(),
            adapter: AdapterId::Cargo,
            recorded_at: 1_700_000_000,
            runs: 3,
            exit: 101,
            totals: Some(Totals {
                passed: 4,
                failed: 1,
                ignored: 0,
            }),
            failures: vec![Stored {
                failure: Failure {
                    identity: Identity {
                        tier: Tier::Constructed,
                        confidence: Confidence::Canonical,
                        key: "src/lib.rs|E0425|frequencies".into(),
                    },
                    severity: Severity::Error,
                    rule: Some("E0425".into()),
                    location: Some(Location {
                        file: "src/lib.rs".into(),
                        line: 4,
                        column: 9,
                    }),
                    message: "cannot find function `frequencies` in this scope".into(),
                    detail: Some("error[E0425]: ...\n".into()),
                },
                history: "FFF".into(),
            }],
        }
    }

    #[test]
    fn root_prefers_the_override_then_xdg_then_home() {
        let o = |s: &str| Some(OsString::from(s));
        assert_eq!(
            root_from(o("/x"), o("/xdg"), o("/home/u")),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(
            root_from(None, o("/xdg"), o("/home/u")),
            Some(PathBuf::from("/xdg/ck"))
        );
        assert_eq!(
            root_from(None, None, o("/home/u")),
            Some(PathBuf::from("/home/u/.cache/ck"))
        );
        assert_eq!(root_from(None, None, None), None);
    }

    #[test]
    fn encoding_is_reversible_and_leaves_plain_names_alone() {
        assert_eq!(encode("main"), "main");
        assert_eq!(encode("feature/thing"), "feature%2Fthing");
        assert_eq!(encode("a b@c"), "a%20b%40c");
        assert_ne!(encode("feature/thing"), encode("feature%2Fthing"));
    }

    #[test]
    fn the_path_reads_as_repo_branch_command() {
        let k = key(Branch::Named("feature/thing".into()), &["cargo", "test"]);
        let path = k.path();
        let parts: Vec<String> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].starts_with("thing-"), "{}", parts[0]);
        assert_eq!(parts[1], "feature%2Fthing");
        assert!(parts[2].starts_with("cargo-test-"), "{}", parts[2]);
        assert!(parts[2].ends_with(".json"));
    }

    #[test]
    fn branches_that_are_not_branches_cannot_collide_with_one() {
        let detached = key(Branch::Detached("abc123".into()), &["x"]);
        let none = key(Branch::NoGit, &["x"]);
        let named = key(Branch::Named("@abc123".into()), &["x"]);
        assert_eq!(detached.branch_segment(), "@abc123");
        assert_eq!(none.branch_segment(), "@no-git");
        assert_eq!(named.branch_segment(), "%40abc123");
    }

    #[test]
    fn the_command_key_sees_argv_assignments_and_directory() {
        let base = key(Branch::Named("main".into()), &["cargo", "test"]);
        let narrowed = key(Branch::Named("main".into()), &["cargo", "test", "parser::"]);
        let with_env = Key::new(
            place(Branch::Named("main".into())),
            &[("RUST_BACKTRACE".into(), "1".into())],
            &["cargo".into(), "test".into()],
        );
        let mut elsewhere = place(Branch::Named("main".into()));
        elsewhere.dir = PathBuf::from("crates/parser");
        let elsewhere = Key::new(elsewhere, &[], &["cargo".into(), "test".into()]);

        let segments: Vec<String> = [&base, &narrowed, &with_env, &elsewhere]
            .iter()
            .map(|k| k.command_segment())
            .collect();
        for (i, a) in segments.iter().enumerate() {
            for b in &segments[i + 1..] {
                assert_ne!(a, b);
            }
        }
        // Same readable prefix throughout; only the hash tells them apart.
        assert!(segments.iter().all(|s| s.starts_with("cargo-test-")));
    }

    #[test]
    fn two_trees_with_the_same_name_do_not_share() {
        let a = key(Branch::Named("main".into()), &["x"]);
        let mut b = a.clone();
        b.place.root = PathBuf::from("/elsewhere/thing");
        assert_ne!(a.repo_segment(), b.repo_segment());
    }

    #[test]
    fn a_saved_baseline_reads_back_identical() {
        let root = scratch();
        let k = key(Branch::Named("main".into()), &["cargo", "test"]);
        let slot = Slot::open_in(&root, &k);
        assert!(slot.writable());
        assert!(matches!(slot.load(), Load::Missing));

        let b = baseline(&k);
        slot.save(&b).unwrap();
        match slot.load() {
            Load::Found(read) => assert_eq!(read, b),
            other => panic!("{other:?}"),
        }
        // Nothing left over from the write.
        let names: Vec<String> = fs::read_dir(slot.path().unwrap().parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.contains(".tmp.")), "{names:?}");
    }

    #[test]
    fn a_held_lock_makes_the_second_opener_read_only() {
        let root = scratch();
        let k = key(Branch::Named("main".into()), &["cargo", "test"]);
        let first = Slot::open_in(&root, &k);
        let second = Slot::open_in(&root, &k);
        assert!(first.writable());
        assert!(!second.writable());
        assert_eq!(second.save(&baseline(&k)), Err(Skipped::Contended));
        // The second can still read what the first wrote.
        first.save(&baseline(&k)).unwrap();
        assert!(matches!(second.load(), Load::Found(_)));
        // And the lock is released with the slot.
        drop(first);
        assert!(Slot::open_in(&root, &k).writable());
    }

    #[test]
    fn a_file_from_another_schema_is_unusable_and_says_so() {
        let root = scratch();
        let k = key(Branch::Named("main".into()), &["cargo", "test"]);
        let slot = Slot::open_in(&root, &k);
        let mut other = serde_json::to_value(baseline(&k)).unwrap();
        other["schema"] = serde_json::Value::from(SCHEMA + 1);
        fs::write(slot.path().unwrap(), other.to_string()).unwrap();
        match slot.load() {
            Load::Unusable(why) => assert!(why.contains("schema"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_torn_or_foreign_file_is_unusable_not_a_crash() {
        let root = scratch();
        let k = key(Branch::Named("main".into()), &["cargo", "test"]);
        let slot = Slot::open_in(&root, &k);
        fs::write(slot.path().unwrap(), "{\"schema\": 1, \"runs\": ").unwrap();
        assert!(matches!(slot.load(), Load::Unusable(_)));
        fs::write(slot.path().unwrap(), "{\"schema\": 1}").unwrap();
        assert!(matches!(slot.load(), Load::Unusable(_)));
        fs::write(slot.path().unwrap(), "[]").unwrap();
        assert!(matches!(slot.load(), Load::Unusable(_)));
    }

    #[test]
    fn a_detached_slot_reads_missing_and_never_writes() {
        let slot = Slot::detached();
        assert!(!slot.writable());
        assert!(matches!(slot.load(), Load::Missing));
        let k = key(Branch::NoGit, &["x"]);
        assert_eq!(slot.save(&baseline(&k)), Err(Skipped::NoStore));
    }

    #[test]
    fn an_unwritable_root_degrades_to_a_detached_slot() {
        let root = scratch().join("file-not-dir");
        fs::write(&root, "").unwrap();
        let slot = Slot::open_in(&root, &key(Branch::NoGit, &["x"]));
        assert!(!slot.writable());
        assert!(matches!(slot.load(), Load::Missing));
    }
}
