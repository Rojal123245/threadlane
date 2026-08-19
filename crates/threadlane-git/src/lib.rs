//! Project-local Git operations used by threadlane workspace.
//!
//! Git is intentionally invoked through the user's configured `git` executable
//! so existing credential helpers, SSH keys, remotes, hooks, and repository
//! configuration continue to work unchanged.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubPrInfo {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub head_ref: String,
    pub base_ref: String,
    pub comments_count: usize,
    pub review_comments: Vec<PrReviewComment>,
    pub checks: Vec<PrCheckStatus>,
    pub total_checks: usize,
    pub failing_checks: usize,
    pub pending_checks: usize,
    pub passing_checks: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrReviewComment {
    pub author: String,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub created_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrCheckStatus {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub details_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: Option<String>,
    detached: bool,
    pub has_upstream: bool,
    pub has_changes: bool,
    pub staged_changes: bool,
    pub unstaged_changes: bool,
    pub ahead: usize,
    behind: usize,
    pub pr_ready: bool,
    pub remote: Option<String>,
    pub branches: Vec<String>,
    pub files: Vec<GitFile>,
    pub pr: Option<GitHubPrInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitFile {
    pub path: String,
    status: String,
    index_status: char,
    worktree_status: char,
    staged: bool,
    pub unstaged: bool,
    pub additions: u32,
    pub deletions: u32,
}

impl GitFile {
    #[cfg_attr(not(test), allow(dead_code))]
    fn status_for_section(&self, staged_section: bool) -> char {
        if staged_section {
            self.index_status
        } else {
            self.worktree_status
        }
    }

    pub fn status_char(&self) -> char {
        if self.index_status != ' ' && self.index_status != '?' {
            self.index_status
        } else if self.worktree_status != ' ' {
            self.worktree_status
        } else {
            'M'
        }
    }
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
    let mut attempts = 0;
    loop {
        attempts += 1;
        let output = Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .output()
            .map_err(|error| GitError {
                work_dir: work_dir.to_path_buf(),
                message: format!("could not start git: {error}"),
            })?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let is_lock_error = stderr.contains("index.lock") || stderr.contains("Unable to create");
        if is_lock_error && attempts <= 5 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: if stderr.is_empty() {
                format!("git exited with {}", output.status)
            } else {
                stderr
            },
        });
    }
}

fn parse_status(_work_dir: &Path, porcelain: &str) -> GitStatus {
    let mut status = GitStatus::default();
    let records = if porcelain.contains('\0') {
        porcelain
            .split('\0')
            .filter(|record| !record.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    } else {
        porcelain.lines().map(str::to_owned).collect::<Vec<_>>()
    };
    let mut records = records.into_iter();
    let header = records
        .next()
        .and_then(|line| line.strip_prefix("## ").map(str::to_owned));
    if let Some(header) = header {
        status.has_upstream = header.contains("...");
        let head = header.split("...").next().unwrap_or(&header);
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

    while let Some(line) = records.next() {
        let bytes = line.as_bytes();
        if bytes.len() < 2 {
            continue;
        }
        let index = bytes[0] as char;
        let worktree = bytes[1] as char;
        status.staged_changes |= index != ' ' && index != '?';
        status.unstaged_changes |= worktree != ' ';
        status.has_changes = true;
        let raw_path = if porcelain.contains('\0') {
            line.get(3..).unwrap_or_default()
        } else {
            line.get(3..).unwrap_or_default().trim()
        };
        // With -z, rename/copy records are followed by the old path as a
        // separate record; the first path is already the new path we display.
        // The line-based fallback keeps the legacy test format readable.
        if (index == 'R' || index == 'C' || worktree == 'R' || worktree == 'C')
            && porcelain.contains('\0')
        {
            let _old_path = records.next();
        }
        let path = raw_path
            .rsplit_once(" -> ")
            .map(|(_, new_path)| new_path)
            .unwrap_or(raw_path)
            .to_owned();
        if !path.is_empty() {
            let status_code = if index == '?' {
                "?".to_owned()
            } else {
                let code = format!("{index}{worktree}");
                code.trim().to_owned()
            };
            status.files.push(GitFile {
                path,
                status: status_code,
                index_status: index,
                worktree_status: worktree,
                staged: index != ' ' && index != '?',
                unstaged: index == '?' || worktree != ' ',
                additions: 0,
                deletions: 0,
            });
        }
    }
    status
}

pub fn inspect_files(work_dir: &Path) -> Result<Vec<GitFile>, GitError> {
    let porcelain = command(work_dir, &["status", "--porcelain=v1", "-b", "-z"])?;
    let mut status = parse_status(work_dir, &porcelain);
    apply_numstats(work_dir, &mut status);
    Ok(status.files)
}

fn apply_numstats(work_dir: &Path, status: &mut GitStatus) {
    let numstat_output = command(work_dir, &["diff", "HEAD", "--numstat"])
        .or_else(|_| command(work_dir, &["diff", "--numstat"]));
    let mut numstats = std::collections::HashMap::new();
    if let Ok(output) = &numstat_output {
        for line in output.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 3 {
                let add = parts[0].parse::<u32>().unwrap_or(0);
                let del = parts[1].parse::<u32>().unwrap_or(0);
                numstats.insert(parts[2].trim().to_owned(), (add, del));
            }
        }
    }
    for file in &mut status.files {
        if let Some(&(add, del)) = numstats.get(&file.path) {
            file.additions = add;
            file.deletions = del;
        } else if file.index_status == '?' || file.worktree_status == '?' {
            if let Ok(content) = std::fs::read_to_string(work_dir.join(&file.path)) {
                let count = content.lines().count() as u32;
                file.additions = if count == 0 { 1 } else { count };
            }
        }
    }
}

pub fn inspect(work_dir: &Path) -> Result<GitStatus, GitError> {
    let porcelain = command(work_dir, &["status", "--porcelain=v1", "-b", "-z"])?;
    let mut status = parse_status(work_dir, &porcelain);
    apply_numstats(work_dir, &mut status);
    status.branches = command(
        work_dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?
    .lines()
    .map(str::trim)
    .filter(|branch| !branch.is_empty())
    .map(str::to_owned)
    .collect();
    if let Some(current_branch) = status.branch.as_ref() {
        if !status
            .branches
            .iter()
            .any(|branch| branch == current_branch)
        {
            status.branches.push(current_branch.clone());
        }
    }
    status.remote = command(work_dir, &["config", "--get", "remote.origin.url"])
        .ok()
        .map(|remote| remote.trim().to_owned())
        .filter(|remote| !remote.is_empty());
    if status.remote.is_some() && status.branch.is_some() {
        let has_upstream = command(
            work_dir,
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        )
        .ok()
        .map(|upstream| !upstream.trim().is_empty())
        .unwrap_or(false);
        if !has_upstream && status.ahead == 0 {
            status.ahead = command(work_dir, &["rev-list", "--count", "HEAD"])
                .ok()
                .and_then(|count| count.trim().parse().ok())
                .unwrap_or(0);
        }
    }
    if let (Some(branch), Some(base)) = (status.branch.as_deref(), default_branch(work_dir)) {
        if branch != base {
            let local_base = command(work_dir, &["rev-list", "--count", &format!("{base}..HEAD")])
                .or_else(|_| {
                    command(
                        work_dir,
                        &["rev-list", "--count", &format!("origin/{base}..HEAD")],
                    )
                });
            status.pr_ready = local_base
                .ok()
                .and_then(|count| count.trim().parse().ok())
                .is_some_and(|count: usize| count > 0);
        }
    }
    status.pr = inspect_pr(work_dir).ok().flatten();
    Ok(status)
}

pub fn parse_gh_pr_json(json_str: &str) -> Result<GitHubPrInfo, String> {
    let val: serde_json::Value = serde_json::from_str(json_str).map_err(|e| e.to_string())?;

    let number = val["number"].as_u64().unwrap_or(0);
    let title = val["title"].as_str().unwrap_or("").to_string();
    let url = val["url"].as_str().unwrap_or("").to_string();
    let state = val["state"].as_str().unwrap_or("").to_string();
    let head_ref = val["headRefName"].as_str().unwrap_or("").to_string();
    let base_ref = val["baseRefName"].as_str().unwrap_or("").to_string();

    let mut review_comments = Vec::new();
    if let Some(comments_arr) = val["comments"].as_array() {
        for item in comments_arr {
            let author = item["author"]["login"]
                .as_str()
                .or_else(|| item["author"].as_str())
                .unwrap_or("unknown")
                .to_string();
            let body = item["body"].as_str().unwrap_or("").to_string();
            let created_at = item["createdAt"].as_str().unwrap_or("").to_string();
            let path = item["path"].as_str().map(|s| s.to_string());
            let line = item["line"].as_u64();
            review_comments.push(PrReviewComment {
                author,
                body,
                path,
                line,
                created_at,
            });
        }
    }
    let comments_count = review_comments.len();

    let mut checks = Vec::new();
    let mut failing_checks = 0;
    let mut pending_checks = 0;
    let mut passing_checks = 0;

    if let Some(checks_arr) = val["statusCheckRollup"].as_array() {
        for check in checks_arr {
            let name = check["name"]
                .as_str()
                .or_else(|| check["context"].as_str())
                .unwrap_or("check")
                .to_string();
            let status = check["status"]
                .as_str()
                .or_else(|| check["state"].as_str())
                .unwrap_or("COMPLETED")
                .to_string();
            let conclusion = check["conclusion"]
                .as_str()
                .or_else(|| check["state"].as_str())
                .map(|s| s.to_string());
            let details_url = check["detailsUrl"]
                .as_str()
                .or_else(|| check["targetUrl"].as_str())
                .map(|s| s.to_string());

            let conclusion_upper = conclusion.as_deref().unwrap_or("").to_uppercase();
            let status_upper = status.to_uppercase();

            if matches!(conclusion_upper.as_str(), "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "ERROR") {
                failing_checks += 1;
            } else if matches!(status_upper.as_str(), "IN_PROGRESS" | "QUEUED" | "PENDING" | "EXPECTED") || conclusion.is_none() {
                pending_checks += 1;
            } else if matches!(conclusion_upper.as_str(), "SUCCESS" | "NEUTRAL" | "SKIPPED") {
                passing_checks += 1;
            }

            checks.push(PrCheckStatus {
                name,
                status,
                conclusion,
                details_url,
            });
        }
    }

    let total_checks = checks.len();

    Ok(GitHubPrInfo {
        number,
        title,
        url,
        state,
        head_ref,
        base_ref,
        comments_count,
        review_comments,
        checks,
        total_checks,
        failing_checks,
        pending_checks,
        passing_checks,
    })
}

pub fn inspect_pr(work_dir: &Path) -> Result<Option<GitHubPrInfo>, GitError> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            "--json",
            "number,title,url,state,comments,statusCheckRollup,headRefName,baseRefName",
        ])
        .current_dir(work_dir)
        .output();

    let Ok(output) = output else {
        return Ok(None);
    };

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Ok(info) = parse_gh_pr_json(&stdout) {
        if info.number > 0 {
            return Ok(Some(info));
        }
    }

    Ok(None)
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

fn create_worktree(work_dir: &Path, path: &Path, branch: &str) -> Result<(), GitError> {
    let branch = validate_branch_name(work_dir, branch)?;
    if !path.is_absolute() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "worktree path must be absolute".to_owned(),
        });
    }
    if path == work_dir {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "worktree path must differ from the current checkout".to_owned(),
        });
    }
    if path.exists() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "worktree path already exists".to_owned(),
        });
    }
    let path = path.to_string_lossy().into_owned();
    command(work_dir, &["worktree", "add", &path, &branch])?;
    Ok(())
}

/// Describe changed paths in dependency-friendly groups for atomic commit planning.
/// Source files are emitted before generated/lock files, and lock files are excluded.
pub fn atomic_commit_groups(work_dir: &Path) -> Result<Vec<Vec<String>>, GitError> {
    let status = inspect(work_dir)?;
    let mut paths = status
        .files
        .into_iter()
        .map(|file| file.path)
        .filter(|path| !path.ends_with("Cargo.lock") && !path.ends_with("package-lock.json"))
        .collect::<Vec<_>>();
    paths.sort_by_key(|path| {
        let generated = path.contains("/target/") || path.ends_with(".generated.rs");
        (generated, path.clone())
    });
    Ok(paths.into_iter().map(|path| vec![path]).collect())
}

/// Stages and commits each planned atomic group. If any group fails, newly
/// staged paths are reset so a caller can review and retry without an accidental
/// combined commit. Previously created commits are intentionally retained.
pub fn commit_atomic_groups(
    work_dir: &Path,
    message_prefix: &str,
) -> Result<Vec<Vec<String>>, GitError> {
    let groups = atomic_commit_groups(work_dir)?;
    if groups.is_empty() {
        return Ok(groups);
    }
    let prefix = message_prefix.trim();
    if prefix.is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "commit message prefix cannot be empty".into(),
        });
    }
    for (index, group) in groups.iter().enumerate() {
        let paths = group.iter().map(String::as_str).collect::<Vec<_>>();
        let mut add_args = vec!["add", "--"];
        add_args.extend(paths);
        if let Err(error) = command(work_dir, &add_args) {
            let _ = command(work_dir, &["reset"]);
            return Err(error);
        }
        if let Err(error) = command(
            work_dir,
            &[
                "commit",
                "-m",
                &format!("{prefix} ({}/{})", index + 1, groups.len()),
            ],
        ) {
            let _ = command(work_dir, &["reset"]);
            return Err(error);
        }
    }
    Ok(groups)
}

pub fn commit_staged(work_dir: &Path, message: &str) -> Result<(), GitError> {
    let message = message.trim();
    if message.is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "commit message cannot be empty".to_owned(),
        });
    }
    command(work_dir, &["commit", "-m", message])?;
    Ok(())
}

pub fn push(work_dir: &Path) -> Result<(), GitError> {
    let has_upstream = command(
        work_dir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .ok()
    .map(|upstream| !upstream.trim().is_empty())
    .unwrap_or(false);
    if has_upstream {
        command(work_dir, &["push"])?;
        return Ok(());
    }
    let branch = command(work_dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    if branch.is_empty() || branch == "HEAD" {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "cannot push a detached HEAD; check out a named branch first".to_owned(),
        });
    }
    command(work_dir, &["push", "--set-upstream", "origin", branch])?;
    Ok(())
}

pub fn pull(work_dir: &Path) -> Result<(), GitError> {
    command(work_dir, &["pull", "--ff-only"])?;
    Ok(())
}

pub fn stage_file(work_dir: &Path, path: &str) -> Result<(), GitError> {
    command(work_dir, &["add", "--", path])?;
    Ok(())
}

pub fn unstage_file(work_dir: &Path, path: &str) -> Result<(), GitError> {
    command(work_dir, &["restore", "--staged", "--", path])?;
    Ok(())
}

fn validate_diff_path(work_dir: &Path, path: &str) -> Result<(), GitError> {
    let invalid = || GitError {
        work_dir: work_dir.to_path_buf(),
        message: format!("path is outside the workspace: {path}"),
    };
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(invalid());
    }

    let root = work_dir.canonicalize().map_err(|error| GitError {
        work_dir: work_dir.to_path_buf(),
        message: format!("could not resolve workspace: {error}"),
    })?;
    let mut existing = work_dir.join(relative);
    while !existing.exists() {
        if !existing.pop() {
            return Err(invalid());
        }
    }
    if !existing.canonicalize().map_err(|error| GitError {
        work_dir: work_dir.to_path_buf(),
        message: format!("could not resolve path: {error}"),
    })?.starts_with(&root) {
        return Err(invalid());
    }
    Ok(())
}

pub fn diff_file(work_dir: &Path, path: &str) -> Result<String, GitError> {
    validate_diff_path(work_dir, path)?;
    // 1. Try diff against HEAD (both staged and unstaged combined)
    if let Ok(head_diff) = command(work_dir, &["diff", "--no-ext-diff", "HEAD", "--", path]) {
        if !head_diff.trim().is_empty() {
            return Ok(head_diff);
        }
    }

    // 2. Try unstaged + staged separately (e.g. if HEAD is unborn or detached)
    let mut diff = String::new();
    let staged_result = command(work_dir, &["diff", "--no-ext-diff", "--cached", "--", path]);
    let staged = staged_result.as_deref().unwrap_or_default();
    if !staged.trim().is_empty() {
        diff.push_str("# Staged changes\n");
        diff.push_str(&staged);
    }
    let unstaged_result = command(work_dir, &["diff", "--no-ext-diff", "--", path]);
    let unstaged = unstaged_result.as_deref().unwrap_or_default();
    if !unstaged.trim().is_empty() {
        if !diff.is_empty() {
            diff.push('\n');
        }
        diff.push_str("# Unstaged changes\n");
        diff.push_str(&unstaged);
    }
    if !diff.trim().is_empty() {
        return Ok(diff);
    }
    if let (Err(staged_error), Err(_unstaged_error)) = (&staged_result, &unstaged_result) {
        return Err(staged_error.clone());
    }

    // 3. If untracked or new file, show whole file as additions via git diff --no-index
    if command(work_dir, &["ls-files", "--error-unmatch", "--", path]).is_err() {
        let null_source = if cfg!(windows) { "NUL" } else { "/dev/null" };
        if let Ok(output) = Command::new("git")
            .args(["diff", "--no-ext-diff", "--no-index", "--", null_source, path])
            .current_dir(work_dir)
            .output()
        {
            let new_file_diff = String::from_utf8_lossy(&output.stdout);
            if !new_file_diff.trim().is_empty() {
                return Ok(new_file_diff.into_owned());
            }
        }
    }

    // 4. Fallback: if file exists on disk and is untracked, synthesize additions
    if command(work_dir, &["ls-files", "--error-unmatch", "--", path]).is_err() {
        let full_path = work_dir.join(path);
        if full_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let mut synth = format!("diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n", content.lines().count());
                for line in content.lines() {
                    synth.push('+');
                    synth.push_str(line);
                    synth.push('\n');
                }
                return Ok(synth);
            }
        }
    }

    Ok("No textual diff available for this file.\n".to_owned())
}

/// Return the changes most likely to be included in the next commit.
///
/// When anything is staged, only the staged diff is returned. Otherwise the
/// working-tree diff is combined with untracked files so message generation
/// also works before the first staging step.
pub fn commit_message_diff(work_dir: &Path) -> Result<String, GitError> {
    let staged = command(work_dir, &["diff", "--cached", "--"])?;
    if !staged.trim().is_empty() {
        return Ok(staged);
    }

    let mut diff = command(work_dir, &["diff", "--"])?;
    let untracked = command(
        work_dir,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    for path in untracked.split('\0').filter(|path| !path.is_empty()) {
        let file_diff = diff_file(work_dir, path)?;
        if !file_diff.trim().is_empty() {
            if !diff.is_empty() {
                diff.push('\n');
            }
            diff.push_str(&file_diff);
        }
    }
    if diff.trim().is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "no changes available for commit message generation".to_owned(),
        });
    }
    Ok(diff)
}

fn default_branch(work_dir: &Path) -> Option<String> {
    if let Some(branch) = command(
        work_dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|value| value.trim().strip_prefix("origin/").map(str::to_owned))
    {
        return Some(branch);
    }
    for candidate in ["main", "master"] {
        if command(
            work_dir,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/remotes/origin/{candidate}"),
            ],
        )
        .is_ok()
        {
            return Some(candidate.to_owned());
        }
    }
    Some("main".to_owned())
}

fn github_repository(remote: &str) -> Option<(String, String)> {
    let normalized = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let path = normalized
        .strip_prefix("https://github.com/")
        .or_else(|| normalized.strip_prefix("http://github.com/"))
        .or_else(|| normalized.strip_prefix("git@github.com:"))
        .or_else(|| normalized.strip_prefix("ssh://git@github.com/"))?;
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let repository = parts.next()?.trim();
    if owner.is_empty() || repository.is_empty() || parts.next().is_some() {
        return None;
    }
    Some((owner.to_owned(), repository.to_owned()))
}

fn github_compare_url(remote: &str, head: &str, base: Option<&str>) -> Option<String> {
    let (owner, repo) = github_repository(remote)?;
    let base = base.unwrap_or("main");
    Some(format!(
        "https://github.com/{owner}/{repo}/compare/{base}...{head}?expand=1"
    ))
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
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn run_git(work_dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn diff_file_uses_builtin_text_diff_when_external_diff_is_configured() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Threadlane"]);
        fs::write(dir.path().join("tracked.txt"), "original\n").unwrap();
        run_git(dir.path(), &["add", "tracked.txt"]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();

        let helper_path = if cfg!(windows) {
            let path = dir.path().join("external-diff.cmd");
            fs::write(&path, "@echo external-diff-sentinel\r\n").unwrap();
            path
        } else {
            let path = dir.path().join("external-diff.sh");
            fs::write(&path, "#!/bin/sh\necho \"external-diff-sentinel\"\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };
        let helper_arg = helper_path.to_str().unwrap();
        run_git(dir.path(), &["config", "diff.external", helper_arg]);

        let diff = diff_file(dir.path(), "tracked.txt").unwrap();

        assert!(!diff.contains("external-diff-sentinel"));
        assert!(diff.contains("-original"));
        assert!(diff.contains("+changed"));
    }

    #[test]
    fn diff_file_does_not_turn_clean_tracked_files_into_new_files() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Threadlane"]);
        fs::write(dir.path().join("tracked.txt"), "unchanged\n").unwrap();
        run_git(dir.path(), &["add", "tracked.txt"]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);

        let diff = diff_file(dir.path(), "tracked.txt").unwrap();

        assert_eq!(diff, "No textual diff available for this file.\n");
    }

    #[test]
    fn diff_file_rejects_paths_outside_workspace() {
        let dir = tempdir().unwrap();

        let error = diff_file(dir.path(), "../outside.txt").unwrap_err();

        assert!(error.message.contains("outside the workspace"));
    }

    #[test]
    fn diff_file_preserves_git_errors() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), "content\n").unwrap();

        let error = diff_file(dir.path(), "file.txt").unwrap_err();

        assert!(!error.message.is_empty());
    }

    #[test]
    fn parses_branch_and_change_state() {
        let status = parse_status(
            Path::new("/tmp/project"),
            "## feature/demo...origin/feature/demo [ahead 2, behind 1]\nM  staged.rs\n M working.rs\nMM mixed.rs\n?? new.rs\n",
        );
        assert_eq!(status.branch.as_deref(), Some("feature/demo"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert!(status.has_upstream);
        assert!(status.staged_changes);
        assert!(status.unstaged_changes);
        assert!(status.has_changes);
        let mixed = status
            .files
            .iter()
            .find(|file| file.path == "mixed.rs")
            .unwrap();
        assert_eq!(mixed.status, "MM");
        assert_eq!(mixed.status_for_section(true), 'M');
        assert_eq!(mixed.status_for_section(false), 'M');
        assert!(mixed.staged);
        assert!(mixed.unstaged);
    }

    #[test]
    fn parses_detached_head() {
        let status = parse_status(Path::new("/tmp/project"), "## HEAD\n");
        assert!(status.detached);
        assert!(status.branch.is_none());
    }

    #[test]
    fn worktree_creation_rejects_relative_and_current_paths() {
        let repo = Path::new("/tmp/project");
        assert!(create_worktree(repo, Path::new("relative"), "main").is_err());
        assert!(create_worktree(repo, repo, "main").is_err());
    }

    #[test]
    fn normalizes_renamed_paths() {
        let status = parse_status(
            Path::new("/tmp/project"),
            "## main\nR  old_name.rs -> new_name.rs\n",
        );
        assert_eq!(status.files[0].path, "new_name.rs");
        assert_eq!(status.files[0].status, "R");
    }

    #[test]
    fn parses_nul_delimited_paths_and_renames() {
        let status = parse_status(
            Path::new("/tmp/project"),
            "## feature/demo\0?? line\nbreak.txt\0R  new name.txt\0old name.txt\0",
        );
        assert_eq!(status.files.len(), 2);
        assert_eq!(status.files[0].path, "line\nbreak.txt");
        assert_eq!(status.files[1].path, "new name.txt");
        assert_eq!(status.files[1].status, "R");
    }

    #[test]
    fn preserves_leading_and_trailing_whitespace_in_nul_paths() {
        let status = parse_status(
            Path::new("/tmp/project"),
            "## feature/demo\0??  leading.txt \0",
        );
        assert_eq!(status.files[0].path, " leading.txt ");
    }

    #[test]
    fn parses_github_remotes() {
        assert_eq!(
            github_repository("git@github.com:owner/repo.git"),
            Some(("owner".to_owned(), "repo".to_owned()))
        );
        assert_eq!(
            github_repository("https://github.com/owner/repo"),
            Some(("owner".to_owned(), "repo".to_owned()))
        );
        assert_eq!(
            github_compare_url(
                "git@github.com:owner/repo.git",
                "enhancements",
                Some("main")
            ),
            Some("https://github.com/owner/repo/compare/main...enhancements?expand=1".to_owned())
        );
    }

    #[test]
    fn atomic_commit_groups_exclude_locks_and_order_sources_first() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        std::fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "lock\n").unwrap();
        let groups = atomic_commit_groups(dir.path()).unwrap();
        assert_eq!(groups, vec![vec!["src.rs".to_string()]]);
    }

    #[test]
    fn atomic_commit_execution_creates_one_commit_per_group() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("initial.txt"), "initial\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        std::fs::write(dir.path().join("first.rs"), "fn first() {}\n").unwrap();
        std::fs::write(dir.path().join("second.rs"), "fn second() {}\n").unwrap();
        let groups = commit_atomic_groups(dir.path(), "atomic changes").unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(
            command(dir.path(), &["rev-list", "--count", "HEAD"])
                .unwrap()
                .trim(),
            "3"
        );
        assert!(!inspect(dir.path()).unwrap().has_changes);
    }

    #[test]
    fn commit_message_diff_prefers_staged_changes_and_includes_untracked_files() {
        let root = std::env::temp_dir().join(format!(
            "threadlane-git-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.email", "threadlane@example.com"]);
        run_git(&root, &["config", "user.name", "Threadlane"]);
        fs::write(root.join("tracked.txt"), "original\n").unwrap();
        run_git(&root, &["add", "tracked.txt"]);
        run_git(&root, &["commit", "-qm", "initial"]);

        fs::write(root.join("tracked.txt"), "staged\n").unwrap();
        run_git(&root, &["add", "tracked.txt"]);
        fs::write(root.join("tracked.txt"), "staged\nunstaged\n").unwrap();
        fs::write(root.join("new.txt"), "new file\n").unwrap();

        let staged = commit_message_diff(&root).unwrap();
        assert!(staged.contains("+staged"));
        assert!(!staged.contains("+unstaged"));
        assert!(!staged.contains("new.txt"));

        run_git(&root, &["restore", "--staged", "tracked.txt"]);
        let working_tree = commit_message_diff(&root).unwrap();
        assert!(working_tree.contains("+unstaged"));
        assert!(working_tree.contains("new.txt"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_github_pr_json_with_checks_and_comments() {
        let sample = r#"{
            "number": 42,
            "title": "Center editor panel",
            "url": "https://github.com/threadlane/threadlane/pull/42",
            "state": "OPEN",
            "headRefName": "center_editor_panel",
            "baseRefName": "main",
            "comments": [
                {
                    "author": { "login": "reviewer1" },
                    "body": "Please double check the layout.",
                    "createdAt": "2026-08-19T00:00:00Z",
                    "path": "src/screens/editor/view.rs",
                    "line": 45
                },
                {
                    "author": { "login": "reviewer2" },
                    "body": "Looks great overall!",
                    "createdAt": "2026-08-19T00:05:00Z"
                },
                {
                    "author": { "login": "bot" },
                    "body": "Benchmark passed.",
                    "createdAt": "2026-08-19T00:10:00Z"
                }
            ],
            "statusCheckRollup": [
                {
                    "name": "cargo-test",
                    "status": "COMPLETED",
                    "conclusion": "FAILURE",
                    "detailsUrl": "https://github.com/threadlane/threadlane/actions/runs/1"
                },
                {
                    "name": "cargo-check",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "detailsUrl": "https://github.com/threadlane/threadlane/actions/runs/2"
                },
                {
                    "name": "e2e-tests",
                    "status": "IN_PROGRESS",
                    "conclusion": null,
                    "detailsUrl": "https://github.com/threadlane/threadlane/actions/runs/3"
                }
            ]
        }"#;

        let pr = parse_gh_pr_json(sample).unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Center editor panel");
        assert_eq!(pr.head_ref, "center_editor_panel");
        assert_eq!(pr.base_ref, "main");
        assert_eq!(pr.comments_count, 3);
        assert_eq!(pr.review_comments.len(), 3);
        assert_eq!(pr.review_comments[0].author, "reviewer1");
        assert_eq!(pr.total_checks, 3);
        assert_eq!(pr.failing_checks, 1);
        assert_eq!(pr.passing_checks, 1);
        assert_eq!(pr.pending_checks, 1);
    }
}
