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
    pub memory_content: Option<String>,
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
                    if let Ok(content) = std::fs::read_to_string(&candidate) {
                        let content = content.trim().to_string();
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
