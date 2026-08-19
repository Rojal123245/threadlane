use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedGitHubRef<'a> {
    pub owner: Option<&'a str>,
    pub repo: Option<&'a str>,
    pub kind: &'a str,
    pub number: &'a str,
}

pub fn parse_github_ref(input: &str) -> Option<ParsedGitHubRef<'_>> {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix("pr://") {
        parse_scheme_or_path("pr", rest)
    } else if let Some(rest) = input.strip_prefix("issue://") {
        parse_scheme_or_path("issue", rest)
    } else if let Some(rest) = input
        .strip_prefix("https://github.com/")
        .or_else(|| input.strip_prefix("http://github.com/"))
    {
        parse_url_path(rest)
    } else {
        None
    }
}

fn parse_scheme_or_path<'a>(default_kind: &'a str, rest: &'a str) -> Option<ParsedGitHubRef<'a>> {
    let rest = rest.trim_matches('/');
    if rest.is_empty() {
        return None;
    }

    if rest.bytes().all(|b| b.is_ascii_digit()) {
        return Some(ParsedGitHubRef {
            owner: None,
            repo: None,
            kind: default_kind,
            number: rest,
        });
    }

    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 3 {
        let owner = parts[0];
        let repo = parts[1];
        if parts.len() >= 4
            && (parts[2] == "pull"
                || parts[2] == "pulls"
                || parts[2] == "issues"
                || parts[2] == "issue")
        {
            let kind = match parts[2] {
                "pull" | "pulls" => "pr",
                _ => "issue",
            };
            let number = parts[3];
            if number.bytes().all(|b| b.is_ascii_digit()) {
                return Some(ParsedGitHubRef {
                    owner: Some(owner),
                    repo: Some(repo),
                    kind,
                    number,
                });
            }
        } else {
            let number = parts[2];
            if number.bytes().all(|b| b.is_ascii_digit()) {
                return Some(ParsedGitHubRef {
                    owner: Some(owner),
                    repo: Some(repo),
                    kind: default_kind,
                    number,
                });
            }
        }
    }

    None
}

fn parse_url_path(rest: &str) -> Option<ParsedGitHubRef<'_>> {
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 4 {
        let owner = parts[0];
        let repo = parts[1];
        let kind = match parts[2] {
            "pull" | "pulls" => "pr",
            "issues" | "issue" => "issue",
            _ => return None,
        };
        let number = parts[3];
        if number.bytes().all(|b| b.is_ascii_digit()) {
            return Some(ParsedGitHubRef {
                owner: Some(owner),
                repo: Some(repo),
                kind,
                number,
            });
        }
    }
    None
}

pub fn github_path(root: &Path, reference: &str) -> String {
    let parsed = match parse_github_ref(reference) {
        Some(p) => p,
        None => {
            return format!(
                "Invalid GitHub reference '{reference}': expected pr://<num>, issue://<num>, or https://github.com/<owner>/<repo>/pull/<num>"
            );
        }
    };

    let (owner, repo) = match (parsed.owner, parsed.repo) {
        (Some(o), Some(r)) => (o.to_string(), r.to_string()),
        _ => match get_github_remote_owner_repo(root) {
            Some((o, r)) => (o, r),
            None => {
                return format!(
                    "{}://{} requires a GitHub origin remote or an explicit owner/repo URL (e.g. https://github.com/owner/repo/pull/{})",
                    parsed.kind, parsed.number, parsed.number
                );
            }
        },
    };

    let endpoint = match parsed.kind {
        "pr" => format!("repos/{owner}/{repo}/pulls/{}", parsed.number),
        "issue" => format!("repos/{owner}/{repo}/issues/{}", parsed.number),
        _ => return format!("Unknown virtual reference scheme '{}://'", parsed.kind),
    };

    match Command::new("gh")
        .args(["api", &endpoint])
        .current_dir(root)
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        Ok(output) => format!(
            "{}://{} GitHub CLI error: {}",
            parsed.kind,
            parsed.number,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => format!(
            "{}://{} requires the authenticated GitHub CLI: {}",
            parsed.kind, parsed.number, error
        ),
    }
}

#[allow(dead_code)]
pub fn github(root: &Path, kind: &str, number: &str) -> String {
    github_path(root, &format!("{kind}://{number}"))
}

fn get_github_remote_owner_repo(root: &Path) -> Option<(String, String)> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    github_owner_repo(&remote).map(|(o, r)| (o.to_string(), r.to_string()))
}

fn github_owner_repo(remote: &str) -> Option<(&str, &str)> {
    let remote = remote.strip_suffix(".git").unwrap_or(remote);
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("https://github.com/"))?;
    path.split_once('/')
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn skill(root: &Path, name: &str) -> String {
    let clean_name = name.trim_matches('/');
    if clean_name.is_empty() {
        return "Error: 'skill://' reference requires a skill name".to_string();
    }

    let mut search_dirs = vec![
        root.join(".threadlane/skills"),
        root.join(".agents/skills"),
        root.join(".pi/skills"),
    ];

    if let Some(home) = dirs_home() {
        search_dirs.push(home.join(".threadlane/skills"));
        search_dirs.push(home.join(".agents/skills"));
        search_dirs.push(home.join(".pi/agent/skills"));
    }

    for dir in search_dirs {
        let candidates = vec![
            dir.join(clean_name),
            dir.join(format!("{clean_name}.md")),
            dir.join(clean_name).join("SKILL.md"),
            dir.join(clean_name).join("skill.md"),
        ];

        for candidate in candidates {
            if candidate.is_file() {
                if let Ok(content) = fs::read_to_string(&candidate) {
                    return content;
                }
            }
        }
    }

    format!("Unknown skill reference '{clean_name}': No skill file found in workspace or user skills directories")
}

pub fn agent(root: &Path, name: &str) -> String {
    let clean_name = name.trim_matches('/');
    if clean_name.is_empty() {
        return "Error: 'agent://' reference requires an agent name".to_string();
    }

    let mut search_dirs = vec![
        root.join(".threadlane/agents"),
        root.join(".agents/agents"),
    ];

    if let Some(home) = dirs_home() {
        search_dirs.push(home.join(".threadlane/agents"));
        search_dirs.push(home.join(".agents/agents"));
    }

    for dir in search_dirs {
        let candidates = vec![
            dir.join(clean_name),
            dir.join(format!("{clean_name}.md")),
        ];

        for candidate in candidates {
            if candidate.is_file() {
                if let Ok(content) = fs::read_to_string(&candidate) {
                    return content;
                }
            }
        }
    }

    format!("Unknown agent reference '{clean_name}': No agent file found in workspace or user agent directories")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_remote_forms() {
        assert_eq!(
            github_owner_repo("git@github.com:owner/repo.git"),
            Some(("owner", "repo"))
        );
        assert_eq!(
            github_owner_repo("https://github.com/owner/repo"),
            Some(("owner", "repo"))
        );
        assert_eq!(github_owner_repo("https://gitlab.com/owner/repo"), None);
    }

    #[test]
    fn parses_github_refs_correctly() {
        assert_eq!(
            parse_github_ref("pr://70"),
            Some(ParsedGitHubRef {
                owner: None,
                repo: None,
                kind: "pr",
                number: "70"
            })
        );
        assert_eq!(
            parse_github_ref("issue://12"),
            Some(ParsedGitHubRef {
                owner: None,
                repo: None,
                kind: "issue",
                number: "12"
            })
        );
        assert_eq!(
            parse_github_ref("pr://wheregmis/threadlane/70"),
            Some(ParsedGitHubRef {
                owner: Some("wheregmis"),
                repo: Some("threadlane"),
                kind: "pr",
                number: "70"
            })
        );
        assert_eq!(
            parse_github_ref("https://github.com/wheregmis/threadlane/pull/70"),
            Some(ParsedGitHubRef {
                owner: Some("wheregmis"),
                repo: Some("threadlane"),
                kind: "pr",
                number: "70"
            })
        );
        assert_eq!(
            parse_github_ref("https://github.com/wheregmis/threadlane/pull/70/files"),
            Some(ParsedGitHubRef {
                owner: Some("wheregmis"),
                repo: Some("threadlane"),
                kind: "pr",
                number: "70"
            })
        );
        assert_eq!(
            parse_github_ref("https://github.com/wheregmis/threadlane/issues/42"),
            Some(ParsedGitHubRef {
                owner: Some("wheregmis"),
                repo: Some("threadlane"),
                kind: "issue",
                number: "42"
            })
        );
    }

    #[test]
    fn discovers_skills_in_subdirectories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        let skill_dir = root.join(".agents/skills/my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "my skill content").unwrap();

        assert_eq!(skill(root, "my-skill"), "my skill content");
    }
}
