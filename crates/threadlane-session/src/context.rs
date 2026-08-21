use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInstruction {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    pub context_files: Vec<PathBuf>,
    pub instructions: Vec<ProjectInstruction>,
    pub combined_instructions: String,
    pub(crate) memory_content: Option<String>,
}

/// Compact a large markdown instruction file by keeping core architectural and repository rules
/// and replacing specialized deep subsystem sections with indexed references.
pub fn scope_instructions(content: &str) -> String {
    if content.len() <= 6144 {
        return content.to_string();
    }

    let mut core_sections = Vec::new();
    let mut subsystem_headings = Vec::new();
    let mut current_heading = String::new();
    let mut current_block = Vec::new();

    let flush_block = |heading: &str, block: &[&str], core: &mut Vec<String>, subs: &mut Vec<String>| {
        let block_str = block.join("\n").trim().to_string();
        if block_str.is_empty() {
            return;
        }
        let heading_lower = heading.to_lowercase();
        let is_subsystem = heading_lower.contains("external acp")
            || heading_lower.contains("wasi extension")
            || heading_lower.contains("background task")
            || heading_lower.contains("model provider")
            || heading_lower.contains("updater")
            || heading_lower.contains("release automation")
            || heading_lower.contains("performance");

        if is_subsystem && block_str.len() > 1000 {
            subs.push(format!("- `{heading}`: Detailed protocol and implementation specifications."));
        } else {
            core.push(block_str);
        }
    };

    for line in content.lines() {
        if line.starts_with("## ") || line.starts_with("# ") {
            if !current_block.is_empty() {
                flush_block(&current_heading, &current_block, &mut core_sections, &mut subsystem_headings);
                current_block.clear();
            }
            current_heading = line.trim_start_matches('#').trim().to_string();
        }
        current_block.push(line);
    }
    if !current_block.is_empty() {
        flush_block(&current_heading, &current_block, &mut core_sections, &mut subsystem_headings);
    }

    if subsystem_headings.is_empty() {
        return content.to_string();
    }

    let mut result = core_sections.join("\n\n");
    result.push_str("\n\n## Subsystem Deep Dive References\n");
    result.push_str("For detailed protocols and subsystem specs, refer to the full instruction file:\n");
    result.push_str(&subsystem_headings.join("\n"));
    result
}

impl ProjectContext {
    pub fn discover(start_dir: &Path) -> Self {
        let mut current = start_dir.to_path_buf();
        let mut context_files = Vec::new();
        let mut instruction_entries = Vec::new();
        let mut combined_instructions = String::new();

        let memory_candidate = start_dir.join(".threadlane").join("memory.md");
        let memory_content = if memory_candidate.is_file() {
            std::fs::read_to_string(&memory_candidate)
                .ok()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
        } else {
            None
        };

        loop {
            for filename in &["AGENTS.md", "THREADLANE.md", ".threadlane/AGENTS.md"] {
                let candidate = current.join(filename);
                if candidate.is_file() {
                    if let Ok(raw_content) = std::fs::read_to_string(&candidate) {
                        let content = scope_instructions(raw_content.trim());
                        combined_instructions.push_str(&format!(
                            "\n--- Context from {} ---\n{}\n",
                            candidate.display(),
                            content
                        ));
                        context_files.push(candidate.clone());
                        instruction_entries.push(ProjectInstruction {
                            path: candidate,
                            content,
                        });
                    }
                }
            }

            if !current.pop() {
                break;
            }
        }

        Self {
            context_files,
            instructions: instruction_entries,
            combined_instructions,
            memory_content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scope_instructions_preserves_short_content() {
        let short = "# Rules\n\n## Repository Map\n\n- crates/foo: foo crate\n";
        assert_eq!(scope_instructions(short), short);
    }

    #[test]
    fn test_scope_instructions_compacts_large_subsystems() {
        let mut large = String::from("# AGENTS.md\n\n## Repository Map\n- crates/core: core crate\n\n## Rust Conventions\n- Keep edits surgical.\n\n");
        large.push_str("## External ACP Agents\n");
        large.push_str(&"ACP protocol details line.\n".repeat(150));
        large.push_str("\n## WASI Extensions\n");
        large.push_str(&"WASI details line.\n".repeat(150));

        let scoped = scope_instructions(&large);
        assert!(scoped.contains("## Repository Map"));
        assert!(scoped.contains("## Rust Conventions"));
        assert!(scoped.contains("## Subsystem Deep Dive References"));
        assert!(scoped.contains("- `External ACP Agents`"));
        assert!(scoped.contains("- `WASI Extensions`"));
        assert!(!scoped.contains("ACP protocol details line.\nACP protocol details line."));
        assert!(scoped.len() < large.len() / 2);
    }
}
