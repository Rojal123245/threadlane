# Built-in Tool Failure Status Design

## Problem

Built-in tools return display text as a `String`, including failure text such as hashline mismatches and file errors. `BuiltinToolExecutor` currently wraps every returned string in `Ok`, so the runtime records `AgentMessage::ToolResult { is_error: false }`. The chat projection consequently treats failed calls as successful and GPUI renders a green check. The same incorrect status is persisted and exposed to every consumer of the runtime projection.

A shell command that exits unsuccessfully has the same problem: `run_command` reports the exit status in text but the executor records the tool call as successful.

## Design

Represent built-in execution outcomes as `Result<String, String>` at the boundary between `threadlane-tools` and `threadlane-runtime`.

- Successful built-in calls return `Ok(output)`.
- Invalid arguments, path validation errors, filesystem failures, hashline edit failures, and other operational failures return `Err(output)`.
- `run_command` returns `Ok(output)` only when the child process exits successfully. A non-zero or signal exit returns `Err(output)` while preserving the existing exit-status, stdout, and stderr detail.
- `BuiltinToolExecutor` forwards this result directly instead of wrapping all output in `Ok`.

The existing runtime flow remains responsible for translating `Err` into `is_error: true`. The existing chat projection then categorizes the activity as `Error`, and the current GPUI renderer displays its red error marker instead of a green check. No GPUI-specific text inspection or rendering workaround is added.

## Compatibility

Keep the public string-returning `execute_tool` and `execute_tool_in_workspace` functions as compatibility wrappers if existing callers or tests rely on them. Add or expose typed execution functions for the runtime executor, with wrappers flattening either outcome back to its display text. This avoids an unnecessary repository-wide API migration while making the canonical agent execution path typed.

Output copy and formatting remain unchanged so model-visible diagnostics and expanded activity details retain their current content.

## Testing

Use test-driven development at the typed execution boundary:

1. A hashline mismatch returns `Err` with the existing diagnostic text.
2. A representative argument or filesystem failure returns `Err`.
3. A shell command with a non-zero exit returns `Err` and retains exit status, stdout, and stderr.
4. Successful built-in and shell calls return `Ok`.
5. A runtime executor test verifies typed built-in failures are not rewrapped as success.

Run focused tests for `threadlane-tools` and `threadlane-runtime`, then the required GPUI check and whitespace validation.

## Scope

This change fixes status classification for all built-in tools and shell exit failures. It does not alter extension-tool contracts, activity layout, icons, output wording, retries, or permission behavior.
