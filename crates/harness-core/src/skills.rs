//! Skills: reusable instruction packs the user drops on disk.
//!
//! Discovery (project shadows global on name collision):
//! - project: `<workspace>/.blurb/skills/`
//! - global:  `~/.config/blurb/skills/` (the host passes the dir in)
//!
//! Each skill is either `<name>.md` or `<name>/SKILL.md`. Optional YAML-ish
//! frontmatter provides `name:` and `description:`; without it the file
//! stem names the skill and its first non-empty line describes it.
//!
//! Progressive disclosure: the system prompt lists names + one-line
//! descriptions (stable, cache-friendly); the `skill` tool loads a skill's
//! full body only when the model decides it applies.

use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillDef {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// "project" or "global" — shown in the prompt so the model knows scope.
    pub scope: &'static str,
}

/// Parse `---\nkey: value\n---` frontmatter. Returns (name, description,
/// body-start-offset); missing keys fall back to defaults.
fn parse_frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let rest = match text.strip_prefix("---") {
        Some(r) => r,
        None => return (None, None),
    };
    let Some(end) = rest.find("\n---") else { return (None, None) };
    let mut name = None;
    let mut description = None;
    for line in rest[..end].lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim() {
                "name" => name = Some(v.trim().trim_matches('"').to_string()),
                "description" => description = Some(v.trim().trim_matches('"').to_string()),
                _ => {}
            }
        }
    }
    (name, description)
}

fn first_line_summary(text: &str) -> String {
    text.lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty() && !l.starts_with("---"))
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

fn load_skill(path: &Path, scope: &'static str) -> Option<SkillDef> {
    let text = std::fs::read_to_string(path).ok()?;
    let (fm_name, fm_desc) = parse_frontmatter(&text);
    let stem = if path.file_name()?.to_str()? == "SKILL.md" {
        path.parent()?.file_name()?.to_str()?.to_string()
    } else {
        path.file_stem()?.to_str()?.to_string()
    };
    let name = fm_name.unwrap_or(stem);
    let description = fm_desc.unwrap_or_else(|| first_line_summary(&text));
    Some(SkillDef { name, description, path: path.to_path_buf(), scope })
}

fn scan_dir(dir: &Path, scope: &'static str, out: &mut Vec<SkillDef>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let nested = path.join("SKILL.md");
            if nested.is_file() {
                if let Some(s) = load_skill(&nested, scope) {
                    out.push(s);
                }
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(s) = load_skill(&path, scope) {
                out.push(s);
            }
        }
    }
}

/// Discover skills for a workspace. Project skills shadow global ones with
/// the same name; results are name-sorted for prompt stability.
pub fn discover(workspace: &Path, global_dir: Option<&Path>) -> Vec<SkillDef> {
    let mut out = Vec::new();
    scan_dir(&workspace.join(".blurb").join("skills"), "project", &mut out);
    if let Some(global) = global_dir {
        let mut globals = Vec::new();
        scan_dir(global, "global", &mut globals);
        for g in globals {
            if !out.iter().any(|s| s.name == g.name) {
                out.push(g);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The "# Skills" section of the system prompt, or None when there are no
/// skills (keeps the prompt byte-stable for skill-less projects).
pub fn prompt_section(skills: &[SkillDef]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut s = String::from(
        "# Skills\nInstruction packs available in this workspace. When a task matches one, load it with the skill tool BEFORE starting that work and follow it.\n",
    );
    for skill in skills {
        s.push_str(&format!("- {} ({}): {}\n", skill.name, skill.scope, skill.description));
    }
    Some(s.trim_end().to_string())
}

/// Loads a skill's full instructions on demand.
pub struct SkillTool {
    def: ToolDef,
    skills: Vec<SkillDef>,
}

impl SkillTool {
    pub fn new(skills: Vec<SkillDef>) -> Self {
        Self {
            def: ToolDef {
                name: "skill".into(),
                description: "Load the full instructions of a skill listed in the system prompt's Skills section. Call it before starting work the skill covers, then follow the loaded instructions.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Exact skill name from the Skills list."}
                    },
                    "required": ["name"]
                }),
            },
            skills,
        }
    }
}

const MAX_SKILL_CHARS: usize = 40_000;

impl Tool for SkillTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }

    fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(name) = input.get("name").and_then(|v| v.as_str()) else {
            return ToolOutput::err("name is required");
        };
        let Some(skill) = self.skills.iter().find(|s| s.name == name) else {
            let known: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
            return ToolOutput::err(format!(
                "unknown skill \"{name}\" — available: {}",
                known.join(", ")
            ));
        };
        match std::fs::read_to_string(&skill.path) {
            Ok(text) => {
                ToolOutput::ok(crate::tools::truncate_output(&text, MAX_SKILL_CHARS))
            }
            Err(e) => ToolOutput::err(format!("failed to read skill: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CancelToken;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn discovers_flat_and_nested_project_shadows_global() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        let global = tmp.path().join("global-skills");

        write(
            &ws.join(".blurb/skills/deploy.md"),
            "---\nname: deploy\ndescription: How we deploy\n---\nStep 1: build.",
        );
        write(
            &ws.join(".blurb/skills/review/SKILL.md"),
            "# Review checklist\nAlways check error paths.",
        );
        write(&global.join("deploy.md"), "GLOBAL deploy — must be shadowed");
        write(&global.join("style.md"), "---\ndescription: House code style\n---\nUse tabs. Just kidding.");

        let skills = discover(&ws, Some(&global));
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["deploy", "review", "style"]);

        let deploy = skills.iter().find(|s| s.name == "deploy").unwrap();
        assert_eq!(deploy.scope, "project");
        assert_eq!(deploy.description, "How we deploy");
        // Nested dir skill named by its directory, described by first line.
        let review = skills.iter().find(|s| s.name == "review").unwrap();
        assert_eq!(review.description, "Review checklist");
        // Frontmatter description without name: stem names it.
        let style = skills.iter().find(|s| s.name == "style").unwrap();
        assert_eq!(style.description, "House code style");
        assert_eq!(style.scope, "global");

        let section = prompt_section(&skills).unwrap();
        assert!(section.contains("- deploy (project): How we deploy"));
        assert!(prompt_section(&[]).is_none());
    }

    #[test]
    fn skill_tool_loads_content_and_rejects_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        write(&ws.join(".blurb/skills/deploy.md"), "---\nname: deploy\n---\nStep 1: build.");
        let skills = discover(&ws, None);
        let tool = SkillTool::new(skills);
        let ctx = ToolContext::new(ws, CancelToken::new());

        let out = tool.execute(&serde_json::json!({"name": "deploy"}), &ctx);
        assert!(!out.is_error);
        assert!(out.content.contains("Step 1: build."));

        let missing = tool.execute(&serde_json::json!({"name": "nope"}), &ctx);
        assert!(missing.is_error);
        assert!(missing.content.contains("available: deploy"));
    }
}
