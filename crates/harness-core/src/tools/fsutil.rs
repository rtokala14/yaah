//! Shared filesystem helpers for tools.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Resolve a model-supplied path against the workspace root and refuse
/// escapes. The path need not exist (write creates files); resolution is
/// purely lexical after joining.
pub fn resolve_safe(cwd: &Path, p: &str) -> Result<PathBuf, String> {
    let joined = if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        cwd.join(p)
    };
    // Lexical normalization (no symlink following for nonexistent paths).
    let mut normal = PathBuf::new();
    for comp in joined.components() {
        match comp {
            std::path::Component::ParentDir => {
                if !normal.pop() {
                    return Err(format!("path escapes workspace root: {p}"));
                }
            }
            std::path::Component::CurDir => {}
            other => normal.push(other),
        }
    }
    if !normal.starts_with(cwd) {
        return Err(format!("path escapes workspace root: {p}"));
    }
    Ok(normal)
}

pub fn mtime_nanos(path: &Path) -> Result<u128, std::io::Error> {
    let meta = std::fs::metadata(path)?;
    Ok(meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0))
}

pub fn str_input<'a>(input: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(|v| v.as_str())
}

pub fn num_input(input: &serde_json::Value, key: &str) -> Option<f64> {
    input.get(key).and_then(|v| v.as_f64())
}
