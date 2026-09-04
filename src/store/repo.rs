//! Where a run happened: the repository, the branch, and the directory
//! within it.
//!
//! Read from the `.git` directory directly rather than by running `git`,
//! because the answer is two file reads and a spawned `git` would cost more
//! than the rest of ck put together. Only `HEAD` is consulted. Nothing here
//! needs the index, the objects, or the configuration, and a working tree
//! that git itself would call broken still has a `HEAD`.
//!
//! A worktree's `.git` is a file naming the real git directory, and the
//! `HEAD` that matters is the one in there: the worktree's own, not the main
//! checkout's. The same file shape serves a submodule.

use std::path::{Path, PathBuf};

/// The repository a run belongs to and the branch it was on.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Place {
    /// The directory the baseline is keyed on: the working tree's top level,
    /// or the current directory itself when there is no git.
    pub root: PathBuf,
    pub branch: Branch,
    /// The current directory relative to `root`. `cargo test` in one
    /// workspace member and `cargo test` in another are different questions,
    /// and this is what keeps them apart.
    pub dir: PathBuf,
}

/// What `HEAD` said. Each variant becomes its own baseline segment: a
/// detached head keys on its commit rather than degrading to first contact
/// on every run, and the same for a directory with no git at all.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Branch {
    Named(String),
    Detached(String),
    NoGit,
}

impl Place {
    /// Locate `cwd` within its repository, if it has one.
    pub fn discover(cwd: &Path) -> Self {
        for ancestor in cwd.ancestors() {
            let dot_git = ancestor.join(".git");
            if let Some(git_dir) = git_dir(&dot_git) {
                return Self {
                    root: ancestor.to_path_buf(),
                    branch: read_head(&git_dir),
                    dir: cwd
                        .strip_prefix(ancestor)
                        .unwrap_or(Path::new(""))
                        .to_path_buf(),
                };
            }
        }
        Self {
            root: cwd.to_path_buf(),
            branch: Branch::NoGit,
            dir: PathBuf::new(),
        }
    }
}

/// The git directory behind a `.git` entry: the entry itself when it is a
/// directory, or the one it names when it is a worktree's pointer file.
fn git_dir(dot_git: &Path) -> Option<PathBuf> {
    if dot_git.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    let pointer = std::fs::read_to_string(dot_git).ok()?;
    let target = pointer.strip_prefix("gitdir:")?.trim();
    let target = Path::new(target);
    // A submodule's pointer is written relative to the directory holding the
    // `.git` file.
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        dot_git.parent().unwrap_or(Path::new("")).join(target)
    })
}

fn read_head(git_dir: &Path) -> Branch {
    let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) else {
        // A `.git` with no readable `HEAD` is not a repository in any state
        // worth keying on. Fall through to the directory itself.
        return Branch::NoGit;
    };
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        Branch::Named(name.to_string())
    } else if !head.is_empty() && head.bytes().all(|b| b.is_ascii_hexdigit()) {
        Branch::Detached(head.to_string())
    } else {
        Branch::NoGit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn scratch() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("ck-repo-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn a_checkout_on_a_branch() {
        let root = scratch();
        write(&root.join(".git/HEAD"), "ref: refs/heads/main\n");
        let place = Place::discover(&root);
        assert_eq!(place.root, root);
        assert_eq!(place.branch, Branch::Named("main".into()));
        assert_eq!(place.dir, PathBuf::new());
    }

    #[test]
    fn a_subdirectory_keeps_its_relative_path() {
        let root = scratch();
        write(&root.join(".git/HEAD"), "ref: refs/heads/feature/thing\n");
        let cwd = root.join("crates/parser");
        fs::create_dir_all(&cwd).unwrap();
        let place = Place::discover(&cwd);
        assert_eq!(place.root, root);
        assert_eq!(place.branch, Branch::Named("feature/thing".into()));
        assert_eq!(place.dir, PathBuf::from("crates/parser"));
    }

    #[test]
    fn a_detached_head_keys_on_its_commit() {
        let root = scratch();
        let hash = "0123456789abcdef0123456789abcdef01234567";
        write(&root.join(".git/HEAD"), &format!("{hash}\n"));
        assert_eq!(Place::discover(&root).branch, Branch::Detached(hash.into()));
    }

    #[test]
    fn a_worktree_reads_its_own_head() {
        let main = scratch();
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(
            &main.join(".git/worktrees/wt/HEAD"),
            "ref: refs/heads/experiment\n",
        );
        let worktree = scratch();
        write(
            &worktree.join(".git"),
            &format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()),
        );
        let place = Place::discover(&worktree);
        assert_eq!(place.root, worktree);
        assert_eq!(place.branch, Branch::Named("experiment".into()));
    }

    #[test]
    fn a_submodule_pointer_is_relative_to_its_own_directory() {
        let root = scratch();
        write(&root.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(
            &root.join(".git/modules/dep/HEAD"),
            "ref: refs/heads/dep-branch\n",
        );
        write(&root.join("dep/.git"), "gitdir: ../.git/modules/dep\n");
        let place = Place::discover(&root.join("dep"));
        assert_eq!(place.root, root.join("dep"));
        assert_eq!(place.branch, Branch::Named("dep-branch".into()));
    }

    #[test]
    fn no_git_keys_on_the_directory_itself() {
        let dir = scratch();
        let place = Place::discover(&dir);
        assert_eq!(place.root, dir);
        assert_eq!(place.branch, Branch::NoGit);
        assert_eq!(place.dir, PathBuf::new());
    }

    #[test]
    fn an_unreadable_head_is_treated_as_no_git() {
        let root = scratch();
        fs::create_dir_all(root.join(".git")).unwrap();
        assert_eq!(Place::discover(&root).branch, Branch::NoGit);
        write(&root.join(".git/HEAD"), "something else entirely\n");
        assert_eq!(Place::discover(&root).branch, Branch::NoGit);
    }
}
