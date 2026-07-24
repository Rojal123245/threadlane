# Design Spec: Hashline Tool & System Prompt Optimization

## Overview

This specification details optimizations for `edit_file_hashline` editing within the Threadlane agent harness. By enhancing tool definitions and system prompt guidelines, models will batch edits into single tool calls, use range anchors for multi-line replacements and deletions, and follow a clear recovery protocol when line hash mismatches occur.

## Proposed Changes

### 1. Tool Schema Definitions (`crates/threadlane-tools/src/lib.rs`)

Enrich the tool description and parameter schema for `edit_file_hashline`:

* **Main Tool Description**:
  ```json
  "Edit a file using hash-anchored lines obtained from read_file. Supports line and range replace, insert_after, and delete operations. Format of start_anchor/end_anchor is 'line_number:hash' (e.g. '12:a3f'). Always batch multiple edits for the same file in one tool call."
  ```

* **Parameter Descriptions**:
  * `edits`: `"List of hash-anchored edit operations to apply atomically (sorted descending automatically by start line)."`
  * `start_anchor`: `"Starting line anchor formatted as 'line_number:hash' (e.g. '12:a3f')."`
  * `end_anchor`: `"Optional ending line anchor for multi-line range edits (e.g. '15:9b2'). If omitted, edit targets single start_anchor line."`
  * `action`: `"Edit action: 'replace' (replaces target line or range with new_content), 'insert_after' (inserts new_content after target line or range), or 'delete' (removes target line or range; new_content omitted/empty)."`
  * `new_content`: `"New replacement or inserted content. Omit or leave empty for 'delete' actions."`

### 2. System Prompt Guidelines (`crates/threadlane-coding-agent/src/system_prompt.rs`)

Expand guidelines injected into the agent system prompt when `edit_file_hashline` is present in `available_tool_names`:

1. `"Prefer edit_file_hashline for high-precision edits using line:hash anchors (e.g. '12:a3f') returned from read_file."`
2. `"For multi-line code blocks or deletions, use range edits (start_anchor and end_anchor) rather than per-line edits."`
3. `"Batch all edits for a file into a single edit_file_hashline tool call's edits array."`
4. `"If a hashline mismatch occurs, re-read the relevant file range with read_file to obtain updated line hashes before retrying."`

### 3. Testing & Verification

* Unit tests in `crates/threadlane-tools`:
  * Verify `get_available_tools()` contains updated descriptions and parameter schema for `edit_file_hashline`.
* Unit tests in `crates/threadlane-coding-agent`:
  * Verify `build_system_prompt` generates all required guidelines when `edit_file_hashline` is in `tools`.

## Risks & Mitigations

* **Token Overhead**: Slightly larger tool schema text is offset immediately by output token savings on multi-line edits and batched calls.
* **Compatibility**: Tool parameter names remain identical (`path`, `edits`, `start_anchor`, `end_anchor`, `action`, `new_content`), ensuring full backwards compatibility with existing deserialization logic in `crates/threadlane-tools/src/hashline.rs`.
