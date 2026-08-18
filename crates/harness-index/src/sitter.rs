//! Tree-sitter symbol extraction — the precision layer. Parses real syntax
//! trees, so strings/comments can't fake definitions and methods know
//! their enclosing type. Languages without a bundled grammar fall back to
//! the regex extractor in `scan.rs`.

use crate::{Symbol, SymbolKind};
use std::path::Path;
use tree_sitter::{Language, Node, Parser};

/// Returns the grammar for a file extension, or None to use the regex
/// fallback.
fn language_for(ext: &str) -> Option<Language> {
    match ext {
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        "js" | "jsx" | "mjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "ts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        _ => None,
    }
}

/// Node kinds that define a symbol, per language family. `inside_type`
/// marks whether an ancestor was a type/impl container (function → method).
fn classify(ext: &str, node_kind: &str, inside_type: bool) -> Option<SymbolKind> {
    match ext {
        "rs" => match node_kind {
            "function_item" | "function_signature_item" if inside_type => {
                Some(SymbolKind::Method)
            }
            "function_item" => Some(SymbolKind::Function),
            "struct_item" => Some(SymbolKind::Struct),
            "enum_item" => Some(SymbolKind::Enum),
            "trait_item" => Some(SymbolKind::Trait),
            "mod_item" => Some(SymbolKind::Module),
            "const_item" | "static_item" => Some(SymbolKind::Constant),
            "type_item" => Some(SymbolKind::TypeAlias),
            _ => None,
        },
        "py" => match node_kind {
            "function_definition" if inside_type => Some(SymbolKind::Method),
            "function_definition" => Some(SymbolKind::Function),
            "class_definition" => Some(SymbolKind::Class),
            _ => None,
        },
        "js" | "jsx" | "mjs" | "ts" | "tsx" => match node_kind {
            "function_declaration" => Some(SymbolKind::Function),
            "class_declaration" => Some(SymbolKind::Class),
            "method_definition" => Some(SymbolKind::Method),
            "interface_declaration" => Some(SymbolKind::Interface),
            "enum_declaration" => Some(SymbolKind::Enum),
            "type_alias_declaration" => Some(SymbolKind::TypeAlias),
            _ => None,
        },
        "go" => match node_kind {
            "function_declaration" => Some(SymbolKind::Function),
            "method_declaration" => Some(SymbolKind::Method),
            "type_spec" => Some(SymbolKind::Struct),
            _ => None,
        },
        _ => None,
    }
}

/// Containers whose nested functions count as methods.
fn is_type_container(ext: &str, node_kind: &str) -> bool {
    matches!(
        (ext, node_kind),
        ("rs", "impl_item")
            | ("rs", "trait_item")
            | ("py", "class_definition")
            | ("js" | "jsx" | "mjs" | "ts" | "tsx", "class_declaration")
            | ("js" | "jsx" | "mjs" | "ts" | "tsx", "class_body")
    )
}

/// The identifier naming a definition node.
fn name_of<'t>(node: &Node<'t>, text: &'t str) -> Option<&'t str> {
    let name_node = node.child_by_field_name("name")?;
    name_node.utf8_text(text.as_bytes()).ok()
}

/// Extract symbols via tree-sitter; None when the extension has no grammar
/// or parsing fails (caller falls back to regex extraction).
pub fn ts_symbols(path: &Path, text: &str) -> Option<Vec<Symbol>> {
    let ext = path.extension()?.to_str()?;
    let language = language_for(ext)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(text, None)?;

    let mut symbols = Vec::new();
    walk(&tree.root_node(), text, ext, false, path, &mut symbols);
    Some(symbols)
}

fn walk(
    node: &Node,
    text: &str,
    ext: &str,
    inside_type: bool,
    path: &Path,
    out: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Top-level JS/TS const/let bindings (incl. `export const`): named
        // by their declarators; arrow-function values count as functions.
        if matches!(ext, "js" | "jsx" | "mjs" | "ts" | "tsx")
            && matches!(child.kind(), "lexical_declaration" | "variable_declaration")
            && matches!(node.kind(), "program" | "export_statement")
        {
            let mut c2 = child.walk();
            for decl in child.children(&mut c2) {
                if decl.kind() != "variable_declarator" {
                    continue;
                }
                if let Some(name) = name_of(&decl, text) {
                    let is_fn = decl
                        .child_by_field_name("value")
                        .map(|v| {
                            matches!(v.kind(), "arrow_function" | "function_expression" | "function")
                        })
                        .unwrap_or(false);
                    push_symbol(
                        &decl,
                        name,
                        if is_fn { SymbolKind::Function } else { SymbolKind::Variable },
                        text,
                        path,
                        out,
                    );
                }
            }
        }
        if let Some(kind) = classify(ext, child.kind(), inside_type) {
            if let Some(name) = name_of(&child, text) {
                push_symbol(&child, name, kind, text, path, out);
            }
        }
        let nested_in_type = inside_type || is_type_container(ext, child.kind());
        walk(&child, text, ext, nested_in_type, path, out);
    }
}

fn push_symbol(
    node: &Node,
    name: &str,
    kind: SymbolKind,
    text: &str,
    path: &Path,
    out: &mut Vec<Symbol>,
) {
    let line = node.start_position().row as u32 + 1;
    let signature: String = text
        .lines()
        .nth(node.start_position().row)
        .unwrap_or("")
        .trim()
        .chars()
        .take(100)
        .collect();
    out.push(Symbol {
        name: name.to_string(),
        kind,
        file: path.to_path_buf(),
        line,
        signature,
        rank: 0.0,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn rust_precision_no_false_positives_from_strings_or_comments() {
        let text = r#"
// fn commented_out() {}
const MSG: &str = "fn fake_in_string() {}";

pub fn real_fn() {}

pub struct Thing;

impl Thing {
    pub fn method_one(&self) {}
}

trait Doer {
    fn do_it(&self);
}
"#;
        let symbols = ts_symbols(&PathBuf::from("x.rs"), text).unwrap();
        let names: Vec<(&str, SymbolKind)> =
            symbols.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert!(names.contains(&("real_fn", SymbolKind::Function)));
        assert!(names.contains(&("Thing", SymbolKind::Struct)));
        assert!(names.contains(&("method_one", SymbolKind::Method)));
        assert!(names.contains(&("Doer", SymbolKind::Trait)));
        assert!(names.contains(&("do_it", SymbolKind::Method)));
        assert!(names.contains(&("MSG", SymbolKind::Constant)));
        // The regex extractor would have caught both of these:
        assert!(!names.iter().any(|(n, _)| *n == "commented_out"));
        assert!(!names.iter().any(|(n, _)| *n == "fake_in_string"));
    }

    #[test]
    fn python_and_typescript_methods_know_their_class() {
        let py = ts_symbols(
            &PathBuf::from("a.py"),
            "class Server:\n    def handle(self):\n        pass\n\ndef top_level():\n    pass\n",
        )
        .unwrap();
        let handle = py.iter().find(|s| s.name == "handle").unwrap();
        assert_eq!(handle.kind, SymbolKind::Method);
        assert_eq!(py.iter().find(|s| s.name == "top_level").unwrap().kind, SymbolKind::Function);

        let ts = ts_symbols(
            &PathBuf::from("a.ts"),
            "interface Shape { area(): number }\nclass Circle {\n  radius = 1;\n  area() { return 3; }\n}\ntype Alias = string;\nfunction free() {}\n",
        )
        .unwrap();
        assert_eq!(ts.iter().find(|s| s.name == "Shape").unwrap().kind, SymbolKind::Interface);
        assert_eq!(ts.iter().find(|s| s.name == "Circle").unwrap().kind, SymbolKind::Class);
        assert_eq!(ts.iter().find(|s| s.name == "area" && s.kind == SymbolKind::Method).is_some(), true);
        assert_eq!(ts.iter().find(|s| s.name == "Alias").unwrap().kind, SymbolKind::TypeAlias);
        assert_eq!(ts.iter().find(|s| s.name == "free").unwrap().kind, SymbolKind::Function);
    }

    #[test]
    fn go_methods_and_unknown_extensions() {
        let go = ts_symbols(
            &PathBuf::from("m.go"),
            "package m\n\ntype Point struct{ X int }\n\nfunc (p Point) Dist() int { return 0 }\n\nfunc Free() {}\n",
        )
        .unwrap();
        assert_eq!(go.iter().find(|s| s.name == "Point").unwrap().kind, SymbolKind::Struct);
        assert_eq!(go.iter().find(|s| s.name == "Dist").unwrap().kind, SymbolKind::Method);
        assert_eq!(go.iter().find(|s| s.name == "Free").unwrap().kind, SymbolKind::Function);
        // No grammar → None → regex fallback path.
        assert!(ts_symbols(&PathBuf::from("x.rb"), "def hi\nend\n").is_none());
    }
}
