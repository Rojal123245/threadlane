use std::path::Path;
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AstSnippet {
    pub file_path: String,
    pub kind: String,
    pub name: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

pub fn parse_rust_ast(file_path: &Path, code: &str) -> Vec<AstSnippet> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::language();
    if parser.set_language(language).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(code, None) else {
        return Vec::new();
    };

    let root = tree.root_node();
    let mut snippets = Vec::new();
    let lines: Vec<&str> = code.lines().collect();

    collect_rust_nodes(root, code, &lines, file_path, &mut snippets);
    snippets
}

fn collect_rust_nodes(
    node: Node,
    code: &str,
    lines: &[&str],
    file_path: &Path,
    out: &mut Vec<AstSnippet>,
) {
    let kind = node.kind();
    if matches!(
        kind,
        "function_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "impl_item"
            | "mod_item"
            | "type_item"
    ) {
        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        let name = node
            .child_by_field_name("name")
            .map(|n| n.utf8_text(code.as_bytes()).unwrap_or("").to_string())
            .unwrap_or_else(|| {
                if kind == "impl_item" {
                    let text = node.utf8_text(code.as_bytes()).unwrap_or("");
                    text.lines().next().unwrap_or("impl").to_string()
                } else {
                    kind.to_string()
                }
            });

        let first_line = lines.get(start_line - 1).copied().unwrap_or("").trim();
        let signature = if first_line.len() > 100 {
            let prefix: String = first_line.chars().take(97).collect();
            format!("{prefix}...")
        } else {
            first_line.to_string()
        };

        let node_lines = &lines[start_line - 1..end_line.min(lines.len())];
        let content = node_lines.join("\n");

        out.push(AstSnippet {
            file_path: file_path.display().to_string(),
            kind: kind.to_string(),
            name,
            signature,
            start_line,
            end_line,
            content,
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_nodes(child, code, lines, file_path, out);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_rust_ast;

    #[test]
    fn truncates_unicode_signatures_without_panicking() {
        let code = format!("fn {}() {{}}", "한".repeat(40));
        let snippets = parse_rust_ast(std::path::Path::new("test.rs"), &code);

        assert_eq!(snippets.len(), 1);
        assert!(snippets[0].signature.ends_with("..."));
    }
}
