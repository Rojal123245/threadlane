use std::fs;
use std::path::Path;

pub fn skill(root: &Path, name: &str) -> String {
    let relative = format!(".threadlane/skills/{name}.md");
    read(root, &relative, "skill")
}

pub fn agent(root: &Path, name: &str) -> String {
    let relative = format!(".threadlane/agents/{name}.md");
    read(root, &relative, "agent")
}

fn read(root: &Path, relative: &str, kind: &str) -> String {
    let path = root.join(relative);
    match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => format!("Unknown {kind} reference '{relative}': {error}"),
    }
}
