# Built-in Tool Failure Status Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record built-in tool failures—including hashline mismatches and non-zero shell exits—as failed tool results so every UI renders an error state rather than a green success check.

**Architecture:** Add typed `try_execute_tool*` entry points in `threadlane-tools` while retaining the current string-returning functions as compatibility wrappers. Built-in operations classify success or failure where each operation executes, and `BuiltinToolExecutor` forwards the typed result directly into the existing runtime pipeline, which already persists `is_error` and projects the GPUI `Error` activity category.

**Tech Stack:** Rust 2021, `std::process`, Tokio async tests, existing `threadlane-tools` and `threadlane-runtime` crates.

## Global Constraints

- Preserve existing tool output wording and details.
- A non-zero or signal shell exit is a failed tool result.
- Do not infer status from display-text prefixes or substrings.
- Keep `execute_tool(name, args) -> String` and `execute_tool_in_workspace(name, args, workspace_root) -> String` as compatibility wrappers.
- Do not change extension-tool contracts, activity layout, icons, retries, or permission behavior.
- Do not modify or discard the unrelated working-tree change in `crates/threadlane-runtime/src/harness/jsonl.rs`.
- Introduce no new dependency.

## File Structure

- Modify `crates/threadlane-tools/src/lib.rs`: own typed built-in execution outcomes, compatibility wrappers, and focused outcome tests.
- Modify `crates/threadlane-runtime/src/tool_executor.rs`: make `BuiltinToolExecutor` forward typed outcomes and test that failures remain failures.
- No GPUI source change is required because `crates/threadlane-runtime/src/harness/projections.rs` and `crates/threadlane-gpui/src/screens/chat/view.rs` already map `is_error: true` to the red `Error` marker.

---

### Task 1: Typed built-in execution outcomes

**Files:**
- Modify: `crates/threadlane-tools/src/lib.rs:334-580`
- Test: `crates/threadlane-tools/src/lib.rs` existing `tests` module

**Interfaces:**
- Consumes: existing built-in tool implementations and `validate_path_in_workspace`.
- Produces: `pub fn try_execute_tool(name: &str, args_json: &str) -> Result<String, String>`.
- Produces: `pub fn try_execute_tool_in_workspace(name: &str, args_json: &str, workspace_root: &Path) -> Result<String, String>`.
- Preserves: `pub fn execute_tool(name: &str, args_json: &str) -> String` and `pub fn execute_tool_in_workspace(name: &str, args_json: &str, workspace_root: &Path) -> String`.

- [ ] **Step 1: Write failing typed-outcome regression tests**

Add tests in `crates/threadlane-tools/src/lib.rs` proving the new API and required classifications:

```rust
#[test]
fn typed_execution_marks_hashline_mismatch_as_error() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("sample.txt"), "original\n").unwrap();
    let result = try_execute_tool_in_workspace(
        "edit_file_hashline",
        &serde_json::json!({
            "path": "sample.txt",
            "edits": [{
                "start_anchor": "1:bad",
                "action": "replace",
                "new_content": "changed"
            }]
        })
        .to_string(),
        dir.path(),
    );

    let error = result.expect_err("a stale hashline anchor must fail");
    assert!(error.contains("Error applying hashline edits"), "{error}");
    assert!(error.contains("Hashline mismatch"), "{error}");
    assert_eq!(fs::read_to_string(dir.path().join("sample.txt")).unwrap(), "original\n");
}

#[test]
fn typed_execution_marks_invalid_arguments_as_error() {
    let result = try_execute_tool("read_file", "{}");
    assert_eq!(result, Err("Error: 'path' parameter is required".into()));
}

#[test]
fn typed_execution_marks_nonzero_command_exit_as_error() {
    let dir = tempdir().unwrap();
    let result = try_execute_tool_in_workspace(
        "run_command",
        r#"{"command":"printf 'out'; printf 'err' >&2; exit 7"}"#,
        dir.path(),
    );

    let error = result.expect_err("a non-zero command exit must fail");
    assert!(error.contains("Exit Status: exit status: 7"), "{error}");
    assert!(error.contains("--- STDOUT ---\nout"), "{error}");
    assert!(error.contains("--- STDERR ---\nerr"), "{error}");
}

#[test]
fn typed_execution_keeps_successful_calls_successful() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("sample.txt"), "contents").unwrap();

    let read = try_execute_tool_in_workspace(
        "read_file",
        r#"{"path":"sample.txt"}"#,
        dir.path(),
    );
    assert!(read.is_ok(), "{read:?}");

    let command = try_execute_tool_in_workspace(
        "run_command",
        r#"{"command":"printf ok"}"#,
        dir.path(),
    );
    assert!(command.is_ok(), "{command:?}");
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p threadlane-tools typed_execution -- --nocapture
```

Expected: compilation fails because `try_execute_tool` and `try_execute_tool_in_workspace` do not exist. This is the intended RED state.

- [ ] **Step 3: Add typed entry points and retain compatibility wrappers**

Change the public entry points to this shape:

```rust
pub fn execute_tool(name: &str, args_json: &str) -> String {
    try_execute_tool(name, args_json).unwrap_or_else(|error| error)
}

pub fn execute_tool_in_workspace(name: &str, args_json: &str, workspace_root: &Path) -> String {
    try_execute_tool_in_workspace(name, args_json, workspace_root)
        .unwrap_or_else(|error| error)
}

pub fn try_execute_tool(name: &str, args_json: &str) -> Result<String, String> {
    try_execute_tool_in_workspace(name, args_json, Path::new("."))
}

pub fn try_execute_tool_in_workspace(
    name: &str,
    args_json: &str,
    workspace_root: &Path,
) -> Result<String, String> {
    let args: Value = serde_json::from_str(args_json)
        .map_err(|error| format!("Error parsing tool arguments JSON: {error}"))?;

    match name {
        // Existing tool branches, now returning Ok(output) or Err(error).
        unknown => Err(format!("Error: Unknown tool '{unknown}'")),
    }
}
```

Convert every existing match branch at its operation boundary, without inspecting generated text:

- Required parameters use `ok_or_else(...)?` and return the exact existing error copy.
- `validate_path_in_workspace` and `validate_cwd_in_workspace` propagate their existing `Err` with `?`.
- `search::grep_search`, file reads/writes, directory reads, JSON edit parsing, and `hashline::apply_hashline_edits` map operational failures to `Err(format!(...))` and successes to `Ok(...)`.
- Deprecated `accept_edit`, unknown tools, unknown memory actions, and missing memory arguments return `Err`.
- Read/list/map/memory helpers that can currently produce failure strings must return `Result<String, String>` internally, or have a typed sibling used by this path; keep compatibility wrappers only where another caller requires their string signature.
- Successful post-edit diagnostics remain part of `Ok` output even when diagnostics report source compilation errors; the file operation itself succeeded.

For `run_command`, build the output once and classify with `ExitStatus::success()`:

```rust
match cmd.output() {
    Ok(output) => {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let rendered = truncate_tool_output(&format!(
            "Exit Status: {}\n--- STDOUT ---\n{}\n--- STDERR ---\n{}",
            output.status, stdout, stderr
        ));
        if output.status.success() {
            Ok(rendered)
        } else {
            Err(rendered)
        }
    }
    Err(error) => Err(format!("Error executing command '{cmd_str}': {error}")),
}
```

- [ ] **Step 4: Run focused and complete tools tests and verify GREEN**

Run:

```bash
cargo test -p threadlane-tools typed_execution -- --nocapture
cargo test -p threadlane-tools
```

Expected: all typed-outcome regression tests pass, then all `threadlane-tools` tests pass.

- [ ] **Step 5: Commit typed tool outcomes**

```bash
git add crates/threadlane-tools/src/lib.rs
git commit -m "fix(tools): return typed built-in failures"
```

---

### Task 2: Forward typed outcomes through the runtime executor

**Files:**
- Modify: `crates/threadlane-runtime/src/tool_executor.rs:4-52`
- Test: `crates/threadlane-runtime/src/tool_executor.rs` new `tests` module

**Interfaces:**
- Consumes: `threadlane_tools::try_execute_tool` and `threadlane_tools::try_execute_tool_in_workspace` from Task 1.
- Produces: `BuiltinToolExecutor` returns `Some(Err(detail))` for built-in failures and `Some(Ok(detail))` for successes through the existing `ToolExecutor` trait.

- [ ] **Step 1: Write failing executor propagation tests**

Append this test module to `crates/threadlane-runtime/src/tool_executor.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{BuiltinToolExecutor, ToolExecutor};
    use tempfile::tempdir;

    #[tokio::test]
    async fn builtin_executor_preserves_tool_failure_status() {
        let executor = BuiltinToolExecutor::new();
        let result = executor
            .execute_tool("read_file", "{}")
            .await
            .expect("the built-in executor handles read_file");

        assert_eq!(result, Err("Error: 'path' parameter is required".into()));
    }

    #[tokio::test]
    async fn builtin_executor_preserves_nonzero_command_status() {
        let dir = tempdir().unwrap();
        let executor = BuiltinToolExecutor::new();
        let result = executor
            .execute_tool_in_workspace(
                "run_command",
                r#"{"command":"exit 9"}"#,
                Some(dir.path()),
            )
            .await
            .expect("the built-in executor handles run_command");

        let error = result.expect_err("a non-zero command exit must remain failed");
        assert!(error.contains("Exit Status: exit status: 9"), "{error}");
    }
}
```

- [ ] **Step 2: Run the focused executor test and verify RED**

Run:

```bash
cargo test -p threadlane-runtime tool_executor::tests -- --nocapture
```

Expected: both tests fail because `BuiltinToolExecutor` currently wraps the string-returning tool output in `Ok`.

- [ ] **Step 3: Forward typed tool results directly**

Replace imports and executor calls with the typed APIs:

```rust
use threadlane_tools::{
    get_available_tools, get_codex_tools, try_execute_tool, try_execute_tool_in_workspace,
};
```

```rust
async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
    Some(try_execute_tool(name, args))
}

async fn execute_tool_in_workspace(
    &self,
    name: &str,
    args: &str,
    work_dir: Option<&std::path::Path>,
) -> Option<Result<String, String>> {
    Some(match work_dir {
        Some(work_dir) => try_execute_tool_in_workspace(name, args, work_dir),
        None => try_execute_tool(name, args),
    })
}
```

- [ ] **Step 4: Run focused and complete runtime tests and verify GREEN**

Run:

```bash
cargo test -p threadlane-runtime tool_executor::tests -- --nocapture
cargo test -p threadlane-runtime
```

Expected: executor propagation tests and all runtime tests pass. The existing dispatcher now receives `Err` and records `is_error: true` without further changes.

- [ ] **Step 5: Commit runtime propagation**

Stage only the intended executor file; do not stage the pre-existing `jsonl.rs` modification:

```bash
git add crates/threadlane-runtime/src/tool_executor.rs
git commit -m "fix(runtime): preserve built-in tool failure status"
```

---

### Task 3: Cross-crate verification

**Files:**
- Verify only; no source change expected.

**Interfaces:**
- Consumes: typed tool outcomes and runtime forwarding from Tasks 1 and 2.
- Produces: evidence that the desktop app compiles against the changed runtime API and the patch has no whitespace errors.

- [ ] **Step 1: Run required desktop-app validation**

```bash
cargo check -p threadlane-gpui
```

Expected: exit status 0. Existing unrelated warnings may remain, but there must be no new error.

- [ ] **Step 2: Run patch whitespace validation**

```bash
git diff --check
```

Expected: exit status 0 and no output.

- [ ] **Step 3: Review scope without disturbing user work**

```bash
git status --short
git diff --stat HEAD~2..HEAD
git diff --name-only HEAD~2..HEAD
```

Expected: the two implementation commits contain only `crates/threadlane-tools/src/lib.rs` and `crates/threadlane-runtime/src/tool_executor.rs`. The unrelated `crates/threadlane-runtime/src/harness/jsonl.rs` modification may still appear in `git status --short` and must remain untouched.

- [ ] **Step 4: Record any durable repository lesson only if newly discovered**

Review `AGENTS.md`. Do not add task-specific guidance. Update it only if implementation reveals a reusable, non-obvious convention not already represented by the design or existing instructions; otherwise make no documentation change.
