//! Per-project session persistence: an index of sessions (titles, worktree
//! bindings, provider labels) plus a journal file per session (written by
//! the session thread in harness-core). Everything lives under the platform
//! data dir — nothing is written into the user's repository.
//!
//! GPUI-free; unit-tested headless.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: u64,
    pub title: String,
    pub provider_label: String,
    /// Worktree name when the session is isolated.
    pub worktree: Option<String>,
    /// The checkout the session runs in (worktree path or project root).
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionIndex {
    #[serde(default = "one")]
    pub next_id: u64,
    #[serde(default)]
    pub sessions: Vec<SessionMeta>,
}

fn one() -> u64 {
    1
}

/// Storage for one project (keyed by a stable hash of its root path).
pub struct ProjectStore {
    dir: PathBuf,
    pub index: SessionIndex,
}

impl ProjectStore {
    /// Store under the platform data dir (~/.local/share/blurb on Linux).
    pub fn open(project_root: &Path) -> Self {
        let base = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("blurb")
            .join("projects");
        Self::open_in(base, project_root)
    }

    /// Testable constructor with an explicit base directory.
    pub fn open_in(base: PathBuf, project_root: &Path) -> Self {
        let dir = base.join(project_key(project_root));
        let index = std::fs::read_to_string(dir.join("sessions.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| SessionIndex { next_id: 1, sessions: Vec::new() });
        Self { dir, index }
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(text) = serde_json::to_string_pretty(&self.index) {
            let tmp = self.dir.join("sessions.json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, self.dir.join("sessions.json"));
            }
        }
    }

    pub fn journal_path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("session-{id}.json"))
    }

    /// Allocate an id and register a session. Caller saves.
    pub fn register(&mut self, meta: SessionMeta) {
        self.index.next_id = self.index.next_id.max(meta.id + 1);
        self.index.sessions.retain(|s| s.id != meta.id);
        self.index.sessions.push(meta);
    }

    pub fn allocate_id(&mut self) -> u64 {
        let id = self.index.next_id;
        self.index.next_id += 1;
        id
    }

    /// Remove a session and its journal file.
    pub fn remove(&mut self, id: u64) {
        self.index.sessions.retain(|s| s.id != id);
        let _ = std::fs::remove_file(self.journal_path(id));
    }

    pub fn update_provider_label(&mut self, id: u64, label: &str) {
        if let Some(s) = self.index.sessions.iter_mut().find(|s| s.id == id) {
            s.provider_label = label.to_string();
        }
    }
}

/// Stable directory name for a project root: last path component + short
/// hash of the full path (readable *and* collision-free enough).
fn project_key(root: &Path) -> String {
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".into());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in root.to_string_lossy().as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{name}-{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(base: &Path) -> ProjectStore {
        ProjectStore::open_in(base.to_path_buf(), Path::new("/home/u/proj"))
    }

    #[test]
    fn index_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = tmp_store(tmp.path());
        let id = store.allocate_id();
        store.register(SessionMeta {
            id,
            title: "fix login".into(),
            provider_label: "Anthropic · claude-opus-5".into(),
            worktree: Some("fix-login-abc".into()),
            cwd: PathBuf::from("/tmp/wt"),
        });
        store.save();

        let reopened = tmp_store(tmp.path());
        assert_eq!(reopened.index.sessions.len(), 1);
        assert_eq!(reopened.index.sessions[0].title, "fix login");
        assert_eq!(reopened.index.next_id, 2);
    }

    #[test]
    fn remove_deletes_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = tmp_store(tmp.path());
        let id = store.allocate_id();
        store.register(SessionMeta {
            id,
            title: "t".into(),
            provider_label: "p".into(),
            worktree: None,
            cwd: PathBuf::from("/x"),
        });
        store.save();
        std::fs::write(store.journal_path(id), "{}").unwrap();
        store.remove(id);
        store.save();
        assert!(!store.journal_path(id).exists());
        assert!(tmp_store(tmp.path()).index.sessions.is_empty());
    }

    #[test]
    fn different_projects_get_different_dirs() {
        let a = project_key(Path::new("/home/u/proj"));
        let b = project_key(Path::new("/home/v/proj"));
        assert_ne!(a, b);
        assert!(a.starts_with("proj-"));
    }
}
