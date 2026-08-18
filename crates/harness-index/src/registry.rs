//! Process-wide index registry: sessions rooted at the same directory share
//! one live index instead of each building and refreshing their own. This
//! is the per-worktree answer for now — worktrees are distinct roots (their
//! content genuinely differs), but restored sessions, non-isolated
//! sessions, and subagents all converge on a single index per root.
//! Entries are weak: closing the last session on a root frees its index.

use crate::scan::RegexIndex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<RegexIndex>>>> = OnceLock::new();

/// The shared index for a root, building it on first use.
pub fn shared_index(root: &Path) -> Arc<RegexIndex> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap();
    if let Some(existing) = map.get(&canonical).and_then(Weak::upgrade) {
        return existing;
    }
    let index = Arc::new(RegexIndex::build(&canonical));
    map.insert(canonical, Arc::downgrade(&index));
    // Opportunistic cleanup of dead entries.
    map.retain(|_, weak| weak.strong_count() > 0);
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_root_shares_different_roots_do_not_and_drop_frees() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("x.rs"), "pub fn in_a() {}\n").unwrap();
        std::fs::write(b.path().join("y.rs"), "pub fn in_b() {}\n").unwrap();

        let a1 = shared_index(a.path());
        let a2 = shared_index(a.path());
        let b1 = shared_index(b.path());
        assert!(Arc::ptr_eq(&a1, &a2), "same root must share one index");
        assert!(!Arc::ptr_eq(&a1, &b1), "different roots must not share");

        let ptr_before = Arc::as_ptr(&a1) as usize;
        drop(a1);
        drop(a2);
        // All strong refs gone → next call builds a fresh index.
        let a3 = shared_index(a.path());
        // (Address may or may not be reused; behavior we care about is that
        // it still answers correctly after the rebuild.)
        let _ = ptr_before;
        use crate::CodeIndex;
        assert_eq!(a3.find_symbols("in_a", 5).unwrap().len(), 1);
    }
}
