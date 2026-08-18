use std::fs;
use std::path::{Path, PathBuf};

pub fn grep_search(root: &Path, pattern: &str, glob: Option<&str>) -> Result<String, String> {
    if pattern.is_empty() {
        return Err("search pattern must not be empty".into());
    }
    let mut files = Vec::new();
    collect_files(root, root, glob, &mut files)?;
    files.sort();
    let mut output = Vec::new();
    for path in files {
        let Ok(content) = fs::read_to_string(&path) else { continue };
        for (index, line) in content.lines().enumerate() {
            if line.contains(pattern) {
                let relative = path.strip_prefix(root).unwrap_or(&path).display();
                output.push(format!("{}:{}:{}", relative, index + 1, line));
            }
        }
    }
    Ok(if output.is_empty() {
        "No matches found.".into()
    } else {
        output.join("\n")
    })
}

fn collect_files(root: &Path, current: &Path, glob: Option<&str>, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(current).map_err(|e| format!("failed to read {}: {e}", current.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" || name == "target" || name == ".threadlane" {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, glob, out)?;
        } else if glob
            .map(|pattern| {
                simple_glob(
                    pattern,
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .as_ref(),
                )
            })
            .unwrap_or(true)
        {
            out.push(path);
        }
    }
    Ok(())
}

fn simple_glob(pattern: &str, value: &str) -> bool {
    match pattern.strip_prefix("**/") {
        Some(suffix) => value.ends_with(suffix),
        None if pattern.starts_with("*.") => value.ends_with(&pattern[1..]),
        None => value == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::grep_search;
    use std::fs;

    #[test]
    fn searches_without_shelling_out() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("one.rs"), "needle\nother\n").unwrap();
        fs::write(dir.path().join("two.txt"), "needle\n").unwrap();
        let result = grep_search(dir.path(), "needle", Some("*.rs")).unwrap();
        assert_eq!(result, "one.rs:1:needle");
    }
}
