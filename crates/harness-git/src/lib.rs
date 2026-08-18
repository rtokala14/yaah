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
use std::collections::HashMap;
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
    /// Recent history across all local branches, lane-assigned for graph
    /// rendering (newest first).
    pub log: Vec<GraphRow>,
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
    pub tip_short_id: String,
    /// Commits ahead/behind the branch's upstream, when it has one.
    pub ahead: Option<usize>,
    pub behind: Option<usize>,
}

/// One commit in the history graph. `lane` is the column of this commit's
/// dot; `active` marks which columns have a line passing through this row
/// (dot column included); `merge_lanes` are columns of extra parents (merge
/// sources) and `closed_lanes` columns that terminated here (branch tips
/// folding into `lane`).
#[derive(Debug, Clone, Serialize)]
pub struct GraphRow {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    pub author: String,
    pub time_unix: i64,
    pub parent_count: usize,
    /// Local branch names whose tip is this commit ("HEAD" branch first).
    pub refs: Vec<String>,
    pub lane: usize,
    pub active: Vec<bool>,
    pub merge_lanes: Vec<usize>,
    pub closed_lanes: Vec<usize>,
}

/// Result of merging a branch into HEAD.
#[derive(Debug, Clone, Serialize)]
pub enum MergeOutcome {
    UpToDate,
    /// HEAD moved forward; payload is the new head short id.
    FastForward(String),
    /// A merge commit was created; payload is its short id.
    Merged(String),
    /// Merge would conflict; the merge was aborted and the working tree
    /// restored. Payload is the conflicted paths.
    Conflicts(Vec<String>),
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
            log: self.log_graph(80)?,
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
            let Some(name) = branch.name()?.map(String::from) else { continue };
            let tip = branch.get().peel_to_commit().ok();
            let tip_short_id =
                tip.as_ref().map(|c| c.id().to_string()[..8].to_string()).unwrap_or_default();
            // Ahead/behind the upstream, when one is configured.
            let (ahead, behind) = match (branch.upstream().ok(), tip.as_ref()) {
                (Some(up), Some(tip)) => match up.get().peel_to_commit() {
                    Ok(up_tip) => self
                        .repo
                        .graph_ahead_behind(tip.id(), up_tip.id())
                        .map(|(a, b)| (Some(a), Some(b)))
                        .unwrap_or((None, None)),
                    Err(_) => (None, None),
                },
                _ => (None, None),
            };
            out.push(BranchInfo {
                is_head: head.as_deref() == Some(name.as_str()),
                name,
                tip_short_id,
                ahead,
                behind,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Recent commits across all local branches, newest first, with graph
    /// lanes assigned (the classic active-lanes algorithm: each lane tracks
    /// the commit id it expects next; a commit claims every lane expecting
    /// it, keeps the first, closes the rest, and hands its lane to its
    /// first parent while extra parents open or join other lanes).
    pub fn log_graph(&self, limit: usize) -> Result<Vec<GraphRow>> {
        // Ref decoration: tip oid -> branch names, HEAD's branch first.
        let head_branch = self.head_branch()?;
        let mut refs: HashMap<git2::Oid, Vec<String>> = HashMap::new();
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        let mut any_pushed = false;
        for entry in self.repo.branches(Some(BranchType::Local))? {
            let (branch, _) = entry?;
            let Some(name) = branch.name()?.map(String::from) else { continue };
            if let Ok(tip) = branch.get().peel_to_commit() {
                let names = refs.entry(tip.id()).or_default();
                if head_branch.as_deref() == Some(name.as_str()) {
                    names.insert(0, name);
                } else {
                    names.push(name);
                }
                if walk.push(tip.id()).is_ok() {
                    any_pushed = true;
                }
            }
        }
        if !any_pushed {
            // Detached or unborn HEAD: walk from HEAD if it exists.
            match self.repo.head().and_then(|h| h.peel_to_commit()) {
                Ok(c) => walk.push(c.id())?,
                Err(_) => return Ok(Vec::new()),
            }
        }

        let mut lanes: Vec<Option<git2::Oid>> = Vec::new();
        let mut rows = Vec::new();
        for oid in walk.take(limit) {
            let oid = oid?;
            let commit = self.repo.find_commit(oid)?;

            // Lanes expecting this commit.
            let expecting: Vec<usize> = lanes
                .iter()
                .enumerate()
                .filter_map(|(i, l)| (*l == Some(oid)).then_some(i))
                .collect();
            let lane = match expecting.first() {
                Some(&i) => i,
                None => {
                    // New tip: first free lane, or a new one.
                    match lanes.iter().position(|l| l.is_none()) {
                        Some(i) => {
                            lanes[i] = Some(oid);
                            i
                        }
                        None => {
                            lanes.push(Some(oid));
                            lanes.len() - 1
                        }
                    }
                }
            };
            let closed_lanes: Vec<usize> = expecting.iter().skip(1).copied().collect();
            for &i in &closed_lanes {
                lanes[i] = None;
            }

            let active: Vec<bool> = lanes.iter().map(|l| l.is_some()).collect();

            // Hand lanes to parents.
            let parents: Vec<git2::Oid> = commit.parent_ids().collect();
            let mut merge_lanes = Vec::new();
            match parents.first() {
                Some(&p) => lanes[lane] = Some(p),
                None => lanes[lane] = None,
            }
            for &p in parents.iter().skip(1) {
                if let Some(i) = lanes.iter().position(|l| *l == Some(p)) {
                    merge_lanes.push(i);
                } else {
                    let i = lanes.iter().position(|l| l.is_none()).unwrap_or_else(|| {
                        lanes.push(None);
                        lanes.len() - 1
                    });
                    lanes[i] = Some(p);
                    merge_lanes.push(i);
                }
            }
            while lanes.last() == Some(&None) {
                lanes.pop();
            }

            rows.push(GraphRow {
                id: oid.to_string(),
                short_id: oid.to_string()[..8].to_string(),
                summary: commit.summary().unwrap_or("").to_string(),
                author: commit.author().name().unwrap_or("").to_string(),
                time_unix: commit.time().seconds(),
                parent_count: parents.len(),
                refs: refs.get(&oid).cloned().unwrap_or_default(),
                lane,
                active,
                merge_lanes,
                closed_lanes,
            });
        }
        Ok(rows)
    }

    /// Merge a local branch into the current HEAD branch: fast-forward when
    /// possible, otherwise a merge commit. On conflicts nothing is left
    /// behind — merge state is cleaned up, the working tree restored — and
    /// the conflicted paths are reported.
    pub fn merge_branch_into_head(&self, branch_name: &str) -> Result<MergeOutcome> {
        let branch = self.repo.find_branch(branch_name, BranchType::Local)?;
        let their_commit = branch.get().peel_to_commit()?;
        let annotated = self.repo.find_annotated_commit(their_commit.id())?;
        let (analysis, _) = self.repo.merge_analysis(&[&annotated])?;

        if analysis.is_up_to_date() {
            return Ok(MergeOutcome::UpToDate);
        }
        if analysis.is_fast_forward() {
            let mut head_ref = self.repo.head()?;
            head_ref.set_target(
                their_commit.id(),
                &format!("blurb: fast-forward merge of {branch_name}"),
            )?;
            self.repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;
            return Ok(MergeOutcome::FastForward(their_commit.id().to_string()[..8].to_string()));
        }

        // Normal merge.
        self.repo.merge(&[&annotated], None, None)?;
        let mut index = self.repo.index()?;
        if index.has_conflicts() {
            let paths: Vec<String> = index
                .conflicts()?
                .filter_map(|c| c.ok())
                .filter_map(|c| {
                    c.our
                        .or(c.their)
                        .or(c.ancestor)
                        .map(|e| String::from_utf8_lossy(&e.path).to_string())
                })
                .collect();
            self.repo.cleanup_state()?;
            let head = self.repo.head()?.peel_to_commit()?;
            self.repo.reset(head.as_object(), git2::ResetType::Hard, None)?;
            return Ok(MergeOutcome::Conflicts(paths));
        }
        let tree_id = index.write_tree_to(&self.repo)?;
        let tree = self.repo.find_tree(tree_id)?;
        let our_commit = self.repo.head()?.peel_to_commit()?;
        let sig = self.repo.signature().or_else(|_| Signature::now("blurb", "blurb@localhost"))?;
        let head_name = self.head_branch()?.unwrap_or_else(|| "HEAD".into());
        let oid = self.repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &format!("Merge branch '{branch_name}' into {head_name}"),
            &tree,
            &[&our_commit, &their_commit],
        )?;
        self.repo.cleanup_state()?;
        self.repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;
        Ok(MergeOutcome::Merged(oid.to_string()[..8].to_string()))
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
        self.diff_patch_inner(None)
    }

    /// Patch text of uncommitted changes for one file.
    pub fn diff_patch_file(&self, path: &str) -> Result<String> {
        self.diff_patch_inner(Some(path))
    }

    fn diff_patch_inner(&self, pathspec: Option<&str>) -> Result<String> {
        let head_tree = self.repo.head().and_then(|h| h.peel_to_tree()).ok();
        let mut opts = DiffOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true).show_untracked_content(true);
        if let Some(p) = pathspec {
            opts.pathspec(p);
        }
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

    fn commit_file(dir: &Path, file: &str, content: &str, msg: &str) -> String {
        let repo = GitRepo::discover(dir).unwrap();
        std::fs::write(dir.join(file), content).unwrap();
        repo.commit_all(msg, "test", "t@example.com").unwrap()
    }

    #[test]
    fn log_graph_linear_history_is_single_lane() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        commit_file(tmp.path(), "b.txt", "b\n", "second");
        commit_file(tmp.path(), "c.txt", "c\n", "third");

        let repo = GitRepo::discover(tmp.path()).unwrap();
        let rows = repo.log_graph(50).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.lane == 0));
        assert_eq!(rows[0].summary, "third");
        assert_eq!(rows[2].summary, "init");
        // Newest commit carries the HEAD branch ref.
        assert!(!rows[0].refs.is_empty());
    }

    #[test]
    fn log_graph_branch_and_merge_uses_second_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let repo2 = init_repo(tmp.path());

        // Branch "feature" from init, commit there, then commit on the
        // original branch, then merge feature back.
        let head = repo2.head().unwrap().peel_to_commit().unwrap();
        repo2.branch("feature", &head, false).unwrap();
        commit_file(tmp.path(), "main.txt", "m\n", "on-main");
        repo2.set_head("refs/heads/feature").unwrap();
        repo2.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();
        commit_file(tmp.path(), "feat.txt", "f\n", "on-feature");
        // Back to the default branch and merge.
        let main_name = {
            let repo = GitRepo::discover(tmp.path()).unwrap();
            repo.branches()
                .unwrap()
                .into_iter()
                .find(|b| b.name != "feature")
                .unwrap()
                .name
        };
        repo2.set_head(&format!("refs/heads/{main_name}")).unwrap();
        repo2.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();

        let repo = GitRepo::discover(tmp.path()).unwrap();
        match repo.merge_branch_into_head("feature").unwrap() {
            MergeOutcome::Merged(_) => {}
            other => panic!("expected Merged, got {other:?}"),
        }

        let rows = repo.log_graph(50).unwrap();
        // Merge commit on top with two parents and a merge lane.
        assert_eq!(rows[0].parent_count, 2);
        assert_eq!(rows[0].merge_lanes.len(), 1);
        // Some row uses a lane other than 0 (the parallel branch).
        assert!(rows.iter().any(|r| r.lane > 0 || r.active.len() > 1));
        // Graph invariant: every dot sits in an active lane.
        assert!(rows.iter().all(|r| r.active.get(r.lane) == Some(&true)));
    }

    #[test]
    fn merge_fast_forward() {
        let tmp = tempfile::tempdir().unwrap();
        let repo2 = init_repo(tmp.path());
        let head = repo2.head().unwrap().peel_to_commit().unwrap();
        repo2.branch("feature", &head, false).unwrap();
        repo2.set_head("refs/heads/feature").unwrap();
        repo2.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();
        let new_id = commit_file(tmp.path(), "f.txt", "f\n", "ff-commit");
        // Return to the (unmoved) default branch.
        let main_name = GitRepo::discover(tmp.path())
            .unwrap()
            .branches()
            .unwrap()
            .into_iter()
            .find(|b| b.name != "feature")
            .unwrap()
            .name;
        repo2.set_head(&format!("refs/heads/{main_name}")).unwrap();
        repo2.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();

        let repo = GitRepo::discover(tmp.path()).unwrap();
        match repo.merge_branch_into_head("feature").unwrap() {
            MergeOutcome::FastForward(id) => assert_eq!(id, new_id),
            other => panic!("expected FastForward, got {other:?}"),
        }
        assert!(tmp.path().join("f.txt").exists());
        // Merging again is a no-op.
        assert!(matches!(
            repo.merge_branch_into_head("feature").unwrap(),
            MergeOutcome::UpToDate
        ));
    }

    #[test]
    fn merge_conflict_aborts_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let repo2 = init_repo(tmp.path());
        let head = repo2.head().unwrap().peel_to_commit().unwrap();
        repo2.branch("feature", &head, false).unwrap();
        // Same file, different content on both branches.
        commit_file(tmp.path(), "a.txt", "main version\n", "main-edit");
        repo2.set_head("refs/heads/feature").unwrap();
        repo2.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();
        commit_file(tmp.path(), "a.txt", "feature version\n", "feature-edit");
        let main_name = GitRepo::discover(tmp.path())
            .unwrap()
            .branches()
            .unwrap()
            .into_iter()
            .find(|b| b.name != "feature")
            .unwrap()
            .name;
        repo2.set_head(&format!("refs/heads/{main_name}")).unwrap();
        repo2.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();

        let repo = GitRepo::discover(tmp.path()).unwrap();
        match repo.merge_branch_into_head("feature").unwrap() {
            MergeOutcome::Conflicts(paths) => assert_eq!(paths, vec!["a.txt".to_string()]),
            other => panic!("expected Conflicts, got {other:?}"),
        }
        // Aborted cleanly: no merge state, working tree back to main.
        assert_eq!(repo2.state(), git2::RepositoryState::Clean);
        assert_eq!(std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(), "main version\n");
        assert!(repo.statuses().unwrap().is_empty());
    }

    #[test]
    fn per_file_patch_is_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "hello\nchanged\n").unwrap();
        std::fs::write(tmp.path().join("new.txt"), "brand new\n").unwrap();

        let repo = GitRepo::discover(tmp.path()).unwrap();
        let a = repo.diff_patch_file("a.txt").unwrap();
        assert!(a.contains("+changed"));
        assert!(!a.contains("brand new"));
        // Untracked files still produce content.
        let n = repo.diff_patch_file("new.txt").unwrap();
        assert!(n.contains("+brand new"));
        // The unscoped patch has both.
        let all = repo.diff_patch().unwrap();
        assert!(all.contains("+changed") && all.contains("+brand new"));
    }

    #[test]
    fn branch_info_carries_tips() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let repo = GitRepo::discover(tmp.path()).unwrap();
        let branches = repo.branches().unwrap();
        assert!(branches.iter().any(|b| b.is_head));
        assert!(branches.iter().all(|b| b.tip_short_id.len() == 8));
        // No upstream configured -> no ahead/behind.
        assert!(branches.iter().all(|b| b.ahead.is_none() && b.behind.is_none()));
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
