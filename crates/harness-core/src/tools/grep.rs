use super::{truncate_output, MAX_OUTPUT_CHARS};
use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use globset::{Glob, GlobMatcher};
use harness_index::CodeIndex;
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct GrepTool {
    def: ToolDef,
    /// When present, patterns with extractable literal tokens search only
    /// the index's candidate files instead of walking the tree.
    index: Option<Arc<dyn CodeIndex>>,
}

impl GrepTool {
    pub fn new() -> Self {
        Self::with_index(None)
    }

    pub fn with_index(index: Option<Arc<dyn CodeIndex>>) -> Self {
        Self {
            def: ToolDef {
                name: "grep".into(),
                description: "Regex search across files in the workspace (gitignore-aware). Returns matching lines as path:line:text. Filter with the glob parameter (e.g. '**/*.rs').".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Rust-flavored regular expression"},
                        "glob": {"type": "string", "description": "Only search files matching this glob"},
                        "max_results": {"type": "number", "description": "default 200"}
                    },
                    "required": ["pattern"]
                }),
            },
            index,
        }
    }
}

/// Identifier tokens that must appear in any line matched by `pattern`, or
/// None when the pattern's structure makes that unsound (alternation,
/// optionality, classes, …). Sound cases: plain literals, anchors, `.`
/// wildcards, and escape sequences — every remaining identifier run of 3+
/// chars is required.
pub fn extract_required_literals(pattern: &str) -> Option<Vec<String>> {
    // Drop escape pairs (\(, \b, \w, …): they impose no unsound structure
    // but their literal is not required text either.
    let mut cleaned = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let _ = chars.next();
            cleaned.push(' ');
        } else if c == '.' || c == '^' || c == '$' {
            // Wildcards/anchors separate tokens but don't make extraction
            // unsound. A quantifier following `.` is consumed as part of
            // the wildcard.
            cleaned.push(' ');
            // peek: consume a quantifier bound to the wildcard
            let rest = chars.as_str();
            if let Some(next) = rest.chars().next() {
                if next == '*' || next == '+' || next == '?' {
                    let _ = chars.next();
                }
            }
        } else {
            cleaned.push(c);
        }
    }
    // Any remaining structural metacharacter → not sound.
    if cleaned.chars().any(|c| "|?*+[](){}".contains(c)) {
        return None;
    }
    let tokens: Vec<String> = cleaned
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| t.len() >= 3 && !t.chars().next().unwrap().is_ascii_digit())
        .map(String::from)
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

impl Tool for GrepTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) else {
            return ToolOutput::err("pattern is required");
        };
        let re = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("invalid regex: {e}")),
        };
        let glob: Option<GlobMatcher> = match input.get("glob").and_then(|v| v.as_str()) {
            Some(g) => match Glob::new(g) {
                Ok(g) => Some(g.compile_matcher()),
                Err(e) => return ToolOutput::err(format!("invalid glob: {e}")),
            },
            None => None,
        };
        let max_results = input
            .get("max_results")
            .and_then(|v| v.as_f64())
            .unwrap_or(200.0)
            .min(1000.0) as usize;

        // Index acceleration: when the pattern carries required literal
        // tokens, only files containing all of them can match.
        let candidates: Option<Vec<std::path::PathBuf>> = self
            .index
            .as_ref()
            .zip(extract_required_literals(pattern))
            .and_then(|(index, tokens)| index.candidate_files(&tokens.join(" ")).ok());

        let mut results: Vec<String> = Vec::new();
        let mut searched = 0usize;
        let mut truncated = false;

        let mut search_file = |rel: &str, abs: &std::path::Path| -> bool {
            if ctx.cancel.is_cancelled() {
                return false;
            }
            if let Some(g) = &glob {
                if !g.is_match(rel) {
                    return true;
                }
            }
            let Ok(text) = std::fs::read_to_string(abs) else { return true };
            if text.contains('\0') {
                return true; // binary
            }
            searched += 1;
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    let shown: String = line.chars().take(300).collect();
                    results.push(format!("{rel}:{}:{shown}", i + 1));
                    if results.len() >= max_results {
                        truncated = true;
                        return false;
                    }
                }
            }
            true
        };

        let accelerated = candidates.is_some();
        match candidates {
            Some(files) => {
                for rel_path in files {
                    let rel = rel_path.to_string_lossy().replace('\\', "/");
                    if !search_file(&rel, &ctx.cwd.join(&rel_path)) {
                        break;
                    }
                }
            }
            None => {
                let walker =
                    WalkBuilder::new(&ctx.cwd).hidden(true).max_filesize(Some(2_000_000)).build();
                for entry in walker.flatten() {
                    if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        continue;
                    }
                    let rel = entry
                        .path()
                        .strip_prefix(&ctx.cwd)
                        .unwrap_or(entry.path())
                        .to_string_lossy()
                        .replace('\\', "/");
                    if !search_file(&rel, entry.path()) {
                        break;
                    }
                }
            }
        }

        if truncated {
            results
                .push(format!("[... hit max_results={max_results}; tighten the pattern or glob]"));
        }
        if results.is_empty() {
            let via = if accelerated { " (index-filtered)" } else { "" };
            ToolOutput::ok(format!("no matches for /{pattern}/ in {searched} files{via}"))
        } else {
            ToolOutput::ok(truncate_output(&results.join("\n"), MAX_OUTPUT_CHARS))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_extraction_is_sound() {
        // Plain literals and escaped metachars are fine.
        assert_eq!(extract_required_literals("mint_token"), Some(vec!["mint_token".into()]));
        assert_eq!(
            extract_required_literals(r"fn mint_token\("),
            Some(vec!["mint_token".into()])
        );
        // Wildcard separators keep both sides as required tokens.
        assert_eq!(
            extract_required_literals("impl.*AuthToken"),
            Some(vec!["impl".into(), "AuthToken".into()])
        );
        assert_eq!(extract_required_literals("^pub fn main"), Some(vec!["pub".into(), "main".into()]));
        assert_eq!(extract_required_literals(r"\bfoo_bar\b"), Some(vec!["foo_bar".into()]));

        // Structural metachars make extraction unsound → None.
        assert_eq!(extract_required_literals("foo|barbaz"), None);
        assert_eq!(extract_required_literals("colou?r"), None);
        assert_eq!(extract_required_literals("ab(cde)+"), None);
        assert_eq!(extract_required_literals("[abc]def"), None);
        // Nothing extractable → None.
        assert_eq!(extract_required_literals("a b"), None);
        assert_eq!(extract_required_literals(r"\d\d\d"), None);
    }

    #[cfg(unix)]
    #[test]
    fn accelerated_and_fallback_search_agree() {
        use crate::types::CancelToken;
        use harness_index::RegexIndex;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "pub fn special_fn() { helper(); }\n").unwrap();
        std::fs::write(tmp.path().join("b.rs"), "fn helper() {}\n").unwrap();
        std::fs::write(tmp.path().join("c.rs"), "fn unrelated() {}\n").unwrap();
        let index: Arc<dyn CodeIndex> = Arc::new(RegexIndex::build(tmp.path()));
        let ctx = ToolContext::new(tmp.path().to_path_buf(), CancelToken::new());

        let plain = GrepTool::new();
        let fast = GrepTool::with_index(Some(index));
        let input = serde_json::json!({"pattern": "special_fn"});
        let a = plain.execute(&input, &ctx);
        let b = fast.execute(&input, &ctx);
        assert!(!a.is_error && !b.is_error);
        assert_eq!(a.content, b.content);
        assert!(b.content.contains("a.rs:1:"));

        // Acceleration stays correct for files created after index build.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(tmp.path().join("late.rs"), "fn late_special_marker() {}\n").unwrap();
        let out = fast.execute(&serde_json::json!({"pattern": "late_special_marker"}), &ctx);
        assert!(out.content.contains("late.rs:1:"), "got: {}", out.content);
    }
}
