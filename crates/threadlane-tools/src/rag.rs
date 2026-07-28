use crate::ast::parse_rust_ast;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstIndexEntry {
    pub file_path: String,
    pub kind: String,
    pub name: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
}

fn calculate_query_score(query_words: &[String], entry: &AstIndexEntry) -> f32 {
    if query_words.is_empty() {
        return 0.0;
    }

    let name_lower = entry.name.to_lowercase();
    let file_lower = entry.file_path.to_lowercase();
    let content_lower = entry.content.to_lowercase();

    let mut score = 0.0f32;
    for word in query_words {
        if name_lower.contains(word) {
            score += 10.0;
        }
        if file_lower.contains(word) {
            score += 5.0;
        }
        if content_lower.contains(word) {
            score += 2.0;
        }
    }

    score
}

pub fn search_codebase_impl(workspace_root: &Path, query: &str, top_k: usize) -> String {
    let mut entries = Vec::new();
    walk_and_index(workspace_root, workspace_root, 0, &mut entries);

    if entries.is_empty() {
        return "No Rust AST nodes found in workspace.".to_string();
    }

    let query_words: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() > 1)
        .map(|w| w.to_string())
        .collect();

    let mut scored: Vec<(f32, &AstIndexEntry)> = entries
        .iter()
        .map(|entry| (calculate_query_score(&query_words, entry), entry))
        .filter(|(score, _)| *score > 0.0)
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    if scored.is_empty() {
        return format!("No matching code nodes found for query: '{query}'");
    }

    let mut results = Vec::new();
    for (score, entry) in scored.into_iter().take(top_k) {
        let content_snippet = if entry.content.lines().count() > 20 {
            let truncated_lines: Vec<&str> = entry.content.lines().take(20).collect();
            format!("{}\n  ... [node content truncated]", truncated_lines.join("\n"))
        } else {
            entry.content.clone()
        };

        results.push(format!(
            "--- Match (Score: {score:.1}) ---\nLocation: {}:{}\nKind: {} | Name: {}\nSignature: {}\nContent:\n{}\n",
            entry.file_path, entry.start_line, entry.kind, entry.name, entry.signature, content_snippet
        ));
    }

    crate::truncate_tool_output(&results.join("\n"))
}

fn walk_and_index(dir: &Path, root: &Path, depth: usize, out: &mut Vec<AstIndexEntry>) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" || name == "dist" {
            continue;
        }

        let path = entry.path();
        if path.is_dir() {
            walk_and_index(&path, root, depth + 1, out);
        } else if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let Ok(code) = fs::read_to_string(&path) else {
                continue;
            };

            let snippets = parse_rust_ast(rel, &code);
            for s in snippets {
                out.push(AstIndexEntry {
                    file_path: s.file_path,
                    kind: s.kind,
                    name: s.name,
                    signature: s.signature,
                    start_line: s.start_line,
                    end_line: s.end_line,
                    content: s.content,
                });
            }
        }
    }
}
