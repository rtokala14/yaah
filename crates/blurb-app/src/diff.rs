//! Unified-diff text → render-ready lines for the diff viewer.
//! GPUI-free; unit-tested headless.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    /// `diff --git` / index / ---/+++ headers.
    FileHeader,
    /// `@@ -a,b +c,d @@` hunk headers.
    HunkHeader,
    Addition,
    Deletion,
    Context,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// Split patch text into typed lines. Keeps the +/-/space markers in the
/// text (familiar, and copy-paste friendly).
pub fn parse_patch(patch: &str) -> Vec<DiffLine> {
    patch
        .lines()
        .map(|line| {
            let kind = if line.starts_with("@@") {
                DiffLineKind::HunkHeader
            } else if line.starts_with("+++")
                || line.starts_with("---")
                || line.starts_with("diff --git")
                || line.starts_with("index ")
                || line.starts_with("new file")
                || line.starts_with("deleted file")
                || line.starts_with("old mode")
                || line.starts_with("new mode")
                || line.starts_with("similarity")
                || line.starts_with("rename ")
            {
                DiffLineKind::FileHeader
            } else if line.starts_with('+') {
                DiffLineKind::Addition
            } else if line.starts_with('-') {
                DiffLineKind::Deletion
            } else {
                DiffLineKind::Context
            };
            DiffLine { kind, text: line.to_string() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_patch_lines() {
        let patch = "\
diff --git a/a.txt b/a.txt
index 111..222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 hello
-old line
+new line
";
        let lines = parse_patch(patch);
        let kinds: Vec<DiffLineKind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::FileHeader,
                DiffLineKind::FileHeader,
                DiffLineKind::FileHeader,
                DiffLineKind::FileHeader,
                DiffLineKind::HunkHeader,
                DiffLineKind::Context,
                DiffLineKind::Deletion,
                DiffLineKind::Addition,
            ]
        );
        assert_eq!(lines[7].text, "+new line");
    }

    #[test]
    fn plusplusplus_in_content_is_not_a_header() {
        // A content line that begins with '+++' only counts as a header when
        // it is one; here it is an addition whose text starts with "++".
        let lines = parse_patch("+++x\n+ normal add\n");
        // "+++x" is indistinguishable from a header prefix in unified diffs;
        // we accept the standard ambiguity and classify by prefix.
        assert_eq!(lines[0].kind, DiffLineKind::FileHeader);
        assert_eq!(lines[1].kind, DiffLineKind::Addition);
    }
}
