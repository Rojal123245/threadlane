//! Project-local Git operations used by the workspace UI.
//!
//! Git is intentionally invoked through the user's configured `git` executable
//! so existing credential helpers, SSH keys, remotes, hooks, and repository
//! configuration continue to work unchanged.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub detached: bool,
    pub has_changes: bool,
    pub staged_changes: bool,
    pub unstaged_changes: bool,
    pub ahead: usize,
    pub behind: usize,
    pub remote: Option<String>,
    pub branches: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitError {
    pub work_dir: PathBuf,
    pub message: String,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.work_dir.display(), self.message)
    }
}

impl std::error::Error for GitError {}

fn command(work_dir: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(work_dir)
        .output()
        .map_err(|error| GitError {
            work_dir: work_dir.to_path_buf(),
            message: format!("could not start git: {error}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: if stderr.is_empty() {
                format!("git exited with {}", output.status)
            } else {
                stderr
            },
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_status(_work_dir: &Path, porcelain: &str) -> GitStatus {
    let mut status = GitStatus::default();
    let mut lines = porcelain.lines();
    if let Some(header) = lines.next().and_then(|line| line.strip_prefix("## ")) {
        let head = header.split("...").next().unwrap_or(header);
        if head == "HEAD" || head.starts_with("(no branch)") {
            status.detached = true;
        } else if !head.is_empty() {
            status.branch = Some(head.to_owned());
        }

        if let Some(ahead_behind) = header
            .split(" [")
            .nth(1)
            .and_then(|value| value.strip_suffix(']'))
        {
            for part in ahead_behind.split(", ") {
                if let Some(value) = part.strip_prefix("ahead ") {
                    status.ahead = value.parse().unwrap_or(0);
                } else if let Some(value) = part.strip_prefix("behind ") {
                    status.behind = value.parse().unwrap_or(0);
                }
            }
        }
    }

    for line in lines {
        let bytes = line.as_bytes();
        if bytes.len() < 2 {
            continue;
        }
        let index = bytes[0] as char;
        let worktree = bytes[1] as char;
        status.staged_changes |= index != ' ' && index != '?';
        status.unstaged_changes |= worktree != ' ';
        status.has_changes = true;
    }
    status
}

pub fn inspect(work_dir: &Path) -> Result<GitStatus, GitError> {
    let porcelain = command(work_dir, &["status", "--porcelain=v1", "-b"])?;
    let mut status = parse_status(work_dir, &porcelain);
    status.branches = command(
        work_dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?
    .lines()
    .filter(|branch| !branch.is_empty())
    .map(str::to_owned)
    .collect();
    status.remote = command(work_dir, &["config", "--get", "remote.origin.url"])
        .ok()
        .map(|remote| remote.trim().to_owned())
        .filter(|remote| !remote.is_empty());
    Ok(status)
}

pub fn create_branch(work_dir: &Path, name: &str) -> Result<(), GitError> {
    let name = validate_branch_name(work_dir, name)?;
    command(work_dir, &["switch", "-c", &name])?;
    Ok(())
}

pub fn checkout(work_dir: &Path, name: &str) -> Result<(), GitError> {
    let name = validate_branch_name(work_dir, name)?;
    command(work_dir, &["switch", &name])?;
    Ok(())
}

pub fn commit_all(work_dir: &Path, message: &str) -> Result<(), GitError> {
    let message = message.trim();
    if message.is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "commit message cannot be empty".to_owned(),
        });
    }
    command(work_dir, &["add", "--all"])?;
    command(work_dir, &["commit", "-m", message])?;
    Ok(())
}

pub fn push(work_dir: &Path) -> Result<(), GitError> {
    command(work_dir, &["push"])?;
    Ok(())
}

fn validate_branch_name(work_dir: &Path, name: &str) -> Result<String, GitError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "branch name cannot be empty".to_owned(),
        });
    }
    command(work_dir, &["check-ref-format", "--branch", name])?;
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_and_change_state() {
        let status = parse_status(
            Path::new("/tmp/project"),
            "## feature/demo...origin/feature/demo [ahead 2, behind 1]\nM  staged.rs\n M working.rs\n?? new.rs\n",
        );
        assert_eq!(status.branch.as_deref(), Some("feature/demo"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert!(status.staged_changes);
        assert!(status.unstaged_changes);
        assert!(status.has_changes);
    }

    #[test]
    fn parses_detached_head() {
        let status = parse_status(Path::new("/tmp/project"), "## HEAD\n");
        assert!(status.detached);
        assert!(status.branch.is_none());
    }
}
