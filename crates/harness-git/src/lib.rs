//! harness-git — native Git integration for the desktop harness.
//!
//! Built on libgit2 (git2 crate): repository state, branches, status, diff
//! summaries, commits, and — central to the app's session model — worktrees.
//!
//! ## The session-per-worktree model
//!
//! Each agent session gets its own git worktree on its own branch
//! (`blurb/<slug>`). Sessions can then run in parallel against the same
//! repository without stepping on each other or on the user's checkout; a
//! finished session's branch is merged (or its worktree discarded) from the
//! git panel. `WorktreeManager` owns creating, listing, and removing these.

use git2::{
    BranchType, DiffOptions, Repository, Signature, StatusOptions, WorktreeAddOptions,
    WorktreePruneOptions,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, GitError>;

// ---------------------------------------------------------------------------
// Read-side snapshots (plain data for the UI)

#[derive(Debug, Clone, Serialize)]
pub struct RepoSnapshot {
    pub root: PathBuf,
    pub head_branch: Option<String>,
    pub head_short_id: String,
    pub head_summary: String,
    pub statuses: Vec<FileStatus>,
    pub branches: Vec<BranchInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub diff: DiffSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileStatus {
    pub path: String,
    /// Two-char porcelain-style code, e.g. "M ", " M", "??", "A ".
    pub code: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeInfo {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_locked: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiffSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub per_file: Vec<FileDiffStat>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDiffStat {
    pub path: String,
    pub insertions: usize,
    pub deletions: usize,
}

// ---------------------------------------------------------------------------

pub struct GitRepo {
    repo: Repository,
    root: PathBuf,
}

impl GitRepo {
    /// Open a repository from any path inside it — including a worktree
    /// checkout (`Repository::discover` handles both).
    pub fn discover(path: &Path) -> Result<Self> {
        let repo = Repository::discover(path)?;
        let root = repo
            .workdir()
            .ok_or_else(|| GitError::Other("bare repository has no workdir".into()))?
            .to_path_buf();
        Ok(Self { repo, root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn snapshot(&self) -> Result<RepoSnapshot> {
        Ok(RepoSnapshot {
            root: self.root.clone(),
            head_branch: self.head_branch()?,
            head_short_id: self.head_short_id()?,
            head_summary: self.head_summary()?,
            statuses: self.statuses()?,
            branches: self.branches()?,
            worktrees: self.worktrees()?,
            diff: self.diff_summary()?,
        })
    }

    pub fn head_branch(&self) -> Result<Option<String>> {
        match self.repo.head() {
            Ok(head) => Ok(head.shorthand().map(String::from).filter(|_| head.is_branch())),
            Err(_) => Ok(None), // unborn HEAD
        }
    }

    fn head_short_id(&self) -> Result<String> {
        match self.repo.head().and_then(|h| h.peel_to_commit()) {
            Ok(c) => Ok(c.id().to_string()[..8.min(40)].to_string()),
            Err(_) => Ok(String::new()),
        }
    }

    fn head_summary(&self) -> Result<String> {
        match self.repo.head().and_then(|h| h.peel_to_commit()) {
            Ok(c) => Ok(c.summary().unwrap_or("").to_string()),
            Err(_) => Ok(String::new()),
        }
    }

    pub fn statuses(&self) -> Result<Vec<FileStatus>> {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true).exclude_submodules(true);
        let statuses = self.repo.statuses(Some(&mut opts))?;
        let mut out = Vec::with_capacity(statuses.len());
        for entry in statuses.iter() {
            let s = entry.status();
            let index_char = if s.is_index_new() {
                'A'
            } else if s.is_index_modified() {
                'M'
            } else if s.is_index_deleted() {
                'D'
            } else if s.is_index_renamed() {
                'R'
            } else {
                ' '
            };
            let wt_char = if s.is_wt_new() {
                '?'
            } else if s.is_wt_modified() {
                'M'
            } else if s.is_wt_deleted() {
                'D'
            } else if s.is_wt_renamed() {
                'R'
            } else {
                ' '
            };
            let code = if s.is_wt_new() {
                "??".to_string()
            } else {
                format!("{index_char}{wt_char}")
            };
            out.push(FileStatus { path: entry.path().unwrap_or("").to_string(), code });
        }
        Ok(out)
    }

    pub fn branches(&self) -> Result<Vec<BranchInfo>> {
        let head = self.head_branch()?;
        let mut out = Vec::new();
        for entry in self.repo.branches(Some(BranchType::Local))? {
            let (branch, _) = entry?;
            if let Some(name) = branch.name()? {
                out.push(BranchInfo {
                    name: name.to_string(),
                    is_head: head.as_deref() == Some(name),
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Uncommitted changes (HEAD tree vs workdir+index).
    pub fn diff_summary(&self) -> Result<DiffSummary> {
        let head_tree = match self.repo.head().and_then(|h| h.peel_to_tree()) {
            Ok(t) => Some(t),
            Err(_) => None,
        };
        let mut opts = DiffOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let diff =
            self.repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
        let stats = diff.stats()?;
        // Two callbacks both need to mutate the accumulator — RefCell keeps
        // the borrow checker satisfied (callbacks never run reentrantly).
        let per_file = std::cell::RefCell::new(Vec::<FileDiffStat>::new());
        diff.foreach(
            &mut |delta, _| {
                let path = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                per_file.borrow_mut().push(FileDiffStat { path, insertions: 0, deletions: 0 });
                true
            },
            None,
            None,
            Some(&mut |_delta, _hunk, line| {
                if let Some(last) = per_file.borrow_mut().last_mut() {
                    match line.origin() {
                        '+' => last.insertions += 1,
                        '-' => last.deletions += 1,
                        _ => {}
                    }
                }
                true
            }),
        )?;
        Ok(DiffSummary {
            files_changed: stats.files_changed(),
            insertions: stats.insertions(),
            deletions: stats.deletions(),
            per_file: per_file.into_inner(),
        })
    }

    /// Full patch text of uncommitted changes (for the diff viewer).
    pub fn diff_patch(&self) -> Result<String> {
        let head_tree = self.repo.head().and_then(|h| h.peel_to_tree()).ok();
        let mut opts = DiffOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let diff =
            self.repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
        let mut out = String::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            match line.origin() {
                '+' | '-' | ' ' => out.push(line.origin()),
                _ => {}
            }
            out.push_str(&String::from_utf8_lossy(line.content()));
            true
        })?;
        Ok(out)
    }

    /// Stage everything and commit using the repo's configured identity.
    pub fn commit_all_default(&self, message: &str) -> Result<String> {
        let sig = self.repo.signature()?;
        let name = sig.name().unwrap_or("blurb").to_string();
        let email = sig.email().unwrap_or("blurb@localhost").to_string();
        self.commit_all(message, &name, &email)
    }

    /// Stage everything and commit. Returns the new commit's short id.
    pub fn commit_all(&self, message: &str, name: &str, email: &str) -> Result<String> {
        let mut index = self.repo.index()?;
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = self.repo.find_tree(tree_id)?;
        let sig = Signature::now(name, email)?;
        let parent = self.repo.head().and_then(|h| h.peel_to_commit()).ok();
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let oid = self.repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
        Ok(oid.to_string()[..8].to_string())
    }

    // -----------------------------------------------------------------------
    // Worktrees

    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let names = self.repo.worktrees()?;
        let mut out = Vec::new();
        for name in names.iter().flatten() {
            let wt = self.repo.find_worktree(name)?;
            let path = wt.path().to_path_buf();
            // Branch of a worktree = HEAD of the repo opened at its path.
            let branch = Repository::open(&path)
                .ok()
                .and_then(|r| r.head().ok().and_then(|h| h.shorthand().map(String::from)));
            out.push(WorktreeInfo {
                name: name.to_string(),
                path,
                branch,
                is_locked: matches!(wt.is_locked(), Ok(git2::WorktreeLockStatus::Locked(_))),
            });
        }
        Ok(out)
    }

    /// Create a worktree at `path` on a new branch `branch_name` forked from
    /// current HEAD. This is the primitive behind session isolation.
    pub fn add_worktree(&self, name: &str, path: &Path, branch_name: &str) -> Result<WorktreeInfo> {
        let head_commit = self.repo.head()?.peel_to_commit()?;
        let branch = self.repo.branch(branch_name, &head_commit, false)?;
        let branch_ref = branch.into_reference();
        let mut opts = WorktreeAddOptions::new();
        opts.reference(Some(&branch_ref));
        let wt = self.repo.worktree(name, path, Some(&opts))?;
        Ok(WorktreeInfo {
            name: name.to_string(),
            path: wt.path().to_path_buf(),
            branch: Some(branch_name.to_string()),
            is_locked: false,
        })
    }

    /// Remove a worktree: delete its checkout directory, then prune the
    /// administrative entry. The branch is left alone (it may hold work).
    pub fn remove_worktree(&self, name: &str) -> Result<()> {
        let wt = self.repo.find_worktree(name)?;
        let path = wt.path().to_path_buf();
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .map_err(|e| GitError::Other(format!("remove {}: {e}", path.display())))?;
        }
        let mut opts = WorktreePruneOptions::new();
        opts.working_tree(true).valid(true);
        wt.prune(Some(&mut opts))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------

/// Manages session worktrees under `<repo>/.blurb/worktrees/`.
pub struct WorktreeManager {
    repo_root: PathBuf,
}

impl WorktreeManager {
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }

    fn container_dir(&self) -> PathBuf {
        // Sibling of the repo, not inside it: a worktree inside the main
        // checkout would show up as untracked noise for the user and agents.
        let name = self
            .repo_root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".into());
        self.repo_root
            .parent()
            .unwrap_or(&self.repo_root)
            .join(format!(".{name}-blurb-worktrees"))
    }

    /// Create an isolated worktree + branch for a session titled `slug`.
    pub fn create_session_worktree(&self, slug: &str) -> Result<WorktreeInfo> {
        let repo = GitRepo::discover(&self.repo_root)?;
        let sanitized: String = slug
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .chars()
            .take(40)
            .collect();
        let unique = format!(
            "{sanitized}-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() & 0xffff)
                .unwrap_or(0)
        );
        let branch = format!("blurb/{unique}");
        let dir = self.container_dir().join(&unique);
        std::fs::create_dir_all(self.container_dir())
            .map_err(|e| GitError::Other(format!("mkdir worktree container: {e}")))?;
        repo.add_worktree(&unique, &dir, &branch)
    }

    pub fn remove_session_worktree(&self, name: &str) -> Result<()> {
        GitRepo::discover(&self.repo_root)?.remove_worktree(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(dir: &Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "test").unwrap();
            cfg.set_str("user.email", "t@example.com").unwrap();
        }
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        {
            let tree = repo.find_tree(tree_id).unwrap();
            let sig = Signature::now("test", "t@example.com").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
        }
        repo
    }

    #[test]
    fn snapshot_and_status() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let repo = GitRepo::discover(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("b.txt"), "new\n").unwrap();
        let snap = repo.snapshot().unwrap();
        assert!(snap.head_branch.is_some());
        assert!(snap.statuses.iter().any(|s| s.path == "b.txt" && s.code == "??"));
    }

    #[test]
    fn worktree_lifecycle() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("proj");
        std::fs::create_dir_all(&repo_dir).unwrap();
        init_repo(&repo_dir);

        let mgr = WorktreeManager::new(repo_dir.clone());
        let wt = mgr.create_session_worktree("Fix Login Bug!").unwrap();
        assert!(wt.path.exists());
        assert!(wt.branch.as_deref().unwrap_or("").starts_with("blurb/fix-login-bug"));

        // The worktree is a usable checkout
        assert!(wt.path.join("a.txt").exists());
        // And discoverable from inside
        let inner = GitRepo::discover(&wt.path).unwrap();
        assert_eq!(inner.head_branch().unwrap().as_deref(), wt.branch.as_deref());

        // Main repo sees it
        let main = GitRepo::discover(&repo_dir).unwrap();
        assert!(main.worktrees().unwrap().iter().any(|w| w.name == wt.name));

        mgr.remove_session_worktree(&wt.name).unwrap();
        assert!(!wt.path.exists());
    }

    #[test]
    fn commit_all_works() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let repo = GitRepo::discover(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("c.txt"), "content\n").unwrap();
        let id = repo.commit_all("add c", "test", "t@example.com").unwrap();
        assert_eq!(id.len(), 8);
        assert!(repo.statuses().unwrap().is_empty());
    }
}
