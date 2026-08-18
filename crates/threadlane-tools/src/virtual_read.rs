use std::fs;
use std::path::Path;
use std::process::Command;

pub fn skill(root: &Path, name: &str) -> String {
    let relative = format!(".threadlane/skills/{name}.md");
    read(root, &relative, "skill")
}

pub fn agent(root: &Path, name: &str) -> String {
    let relative = format!(".threadlane/agents/{name}.md");
    read(root, &relative, "agent")
}

pub fn github(root: &Path, kind: &str, number: &str) -> String {
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return format!("Invalid {kind} reference '{number}': expected a numeric identifier");
    }
    let endpoint = match kind {
        "pr" => format!("repos/{{owner}}/{{repo}}/pulls/{number}"),
        "issue" => format!("repos/{{owner}}/{{repo}}/issues/{number}"),
        _ => return format!("Unknown virtual reference scheme '{kind}://'"),
    };
    let remote = match Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
    {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        Ok(_) => return format!("{kind}:// requires a GitHub origin remote"),
        Err(error) => return format!("{kind}:// could not inspect Git origin: {error}"),
    };
    let Some((owner, repo)) = github_owner_repo(&remote) else {
        return format!("{kind}:// requires a GitHub origin remote");
    };
    let endpoint = endpoint.replace("{owner}", owner).replace("{repo}", repo);
    match Command::new("gh")
        .args(["api", &endpoint])
        .current_dir(root)
        .output()
    {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).into_owned(),
        Ok(output) => format!("{kind}://{number} GitHub CLI error: {}", String::from_utf8_lossy(&output.stderr).trim()),
        Err(error) => format!("{kind}://{number} requires the authenticated GitHub CLI: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::github_owner_repo;

    #[test]
    fn parses_github_remote_forms() {
        assert_eq!(github_owner_repo("git@github.com:owner/repo.git"), Some(("owner", "repo")));
        assert_eq!(github_owner_repo("https://github.com/owner/repo"), Some(("owner", "repo")));
        assert_eq!(github_owner_repo("https://gitlab.com/owner/repo"), None);
    }
}

fn github_owner_repo(remote: &str) -> Option<(&str, &str)> {
    let remote = remote.strip_suffix(".git").unwrap_or(remote);
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("https://github.com/"))?;
    path.split_once('/')
}

fn read(root: &Path, relative: &str, kind: &str) -> String {
    let path = root.join(relative);
    match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => format!("Unknown {kind} reference '{relative}': {error}"),
    }
}
