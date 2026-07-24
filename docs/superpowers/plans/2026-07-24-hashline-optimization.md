# Hashline Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Optimize tool schema descriptions and system prompt guidelines for `edit_file_hashline` to improve edit precision, promote batching and range operations, and ensure reliable mismatch recovery.

**Architecture:** Enrich the `edit_file_hashline` tool definition in `crates/threadlane-tools/src/lib.rs` with detailed parameter and usage descriptions. Expand dynamic guideline generation in `crates/threadlane-coding-agent/src/system_prompt.rs` when `edit_file_hashline` is present in available tools. Update repository instructions in `AGENTS.md`.

**Tech Stack:** Rust (Makepad environment), `serde_json`, `threadlane-tools`, `threadlane-coding-agent`.

## Global Constraints

- Edits must remain focused, surgical, and preserve existing code and functionality.
- Tool schemas must maintain JSON compatibility with existing deserialization structures in `crates/threadlane-tools/src/hashline.rs`.
- `cargo check -p threadlane` and `git diff --check` must pass.

---

### Task 1: Enrich Tool Schema & Descriptions in `threadlane-tools`

**Files:**
- Modify: `crates/threadlane-tools/src/lib.rs:48-72`
- Test: `crates/threadlane-tools/src/lib.rs:442+`

**Interfaces:**
- Consumes: `tool_definitions()` in `crates/threadlane-tools/src/lib.rs`
- Produces: Enriched `edit_file_hashline` tool definition JSON returned by `get_available_tools()` and `get_codex_tools()`

- [ ] **Step 1: Write failing unit test for tool schema content**

Add `test_edit_file_hashline_schema_description` to `crates/threadlane-tools/src/lib.rs` inside `mod tests`:

```rust
    #[test]
    fn test_edit_file_hashline_schema_description() {
        let tools = get_available_tools();
        let hashline_tool = tools
            .iter()
            .find(|t| t["function"]["name"] == "edit_file_hashline")
            .expect("edit_file_hashline tool should exist");
        
        let desc = hashline_tool["function"]["description"].as_str().unwrap();
        assert!(desc.contains("Supports line and range replace, insert_after, and delete operations"));
        assert!(desc.contains("Always batch multiple edits for the same file in one tool call"));

        let params = &hashline_tool["function"]["parameters"]["properties"];
        let start_anchor_desc = params["edits"]["items"]["properties"]["start_anchor"]["description"].as_str().unwrap();
        assert!(start_anchor_desc.contains("formatted as 'line_number:hash'"));

        let end_anchor_desc = params["edits"]["items"]["properties"]["end_anchor"]["description"].as_str().unwrap();
        assert!(end_anchor_desc.contains("multi-line range edits"));

        let action_desc = params["edits"]["items"]["properties"]["action"]["description"].as_str().unwrap();
        assert!(action_desc.contains("'replace'"));
        assert!(action_desc.contains("'insert_after'"));
        assert!(action_desc.contains("'delete'"));

        let new_content_desc = params["edits"]["items"]["properties"]["new_content"]["description"].as_str().unwrap();
        assert!(new_content_desc.contains("Omit or leave empty for 'delete' actions"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p threadlane-tools test_edit_file_hashline_schema_description`
Expected: FAIL with missing assertion strings.

- [ ] **Step 3: Update `edit_file_hashline` tool definition in `crates/threadlane-tools/src/lib.rs`**

Replace lines 48-72 of `crates/threadlane-tools/src/lib.rs` with:

```rust
        json!({
            "name": "edit_file_hashline",
            "description": "Edit a file using hash-anchored lines obtained from read_file. Supports line and range replace, insert_after, and delete operations. Format of start_anchor/end_anchor is 'line_number:hash' (e.g. '12:a3f'). Always batch multiple edits for the same file in one tool call.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to file to edit" },
                    "edits": {
                        "type": "array",
                        "description": "List of hash-anchored edit operations to apply atomically (sorted descending automatically by start line).",
                        "items": {
                            "type": "object",
                            "properties": {
                                "start_anchor": { "type": "string", "description": "Starting line anchor formatted as 'line_number:hash' (e.g. '12:a3f')." },
                                "end_anchor": { "type": "string", "description": "Optional ending line anchor for multi-line range edits (e.g. '15:9b2'). If omitted, edit targets single start_anchor line." },
                                "action": { "type": "string", "enum": ["replace", "insert_after", "delete"], "description": "Edit action: 'replace' (replaces target line or range with new_content), 'insert_after' (inserts new_content after target line or range), or 'delete' (removes target line or range; new_content omitted/empty)." },
                                "new_content": { "type": "string", "description": "New replacement or inserted content. Omit or leave empty for 'delete' actions." }
                            },
                            "required": ["start_anchor", "action"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }
        }),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p threadlane-tools test_edit_file_hashline_schema_description`
Expected: PASS

- [ ] **Step 5: Run all `threadlane-tools` tests**

Run: `cargo test -p threadlane-tools`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-tools/src/lib.rs
git commit -m "feat(tools): enrich edit_file_hashline tool description and parameter schema"
```

---

### Task 2: Expand System Prompt Guidelines in `threadlane-coding-agent`

**Files:**
- Modify: `crates/threadlane-coding-agent/src/system_prompt.rs:128-130`
- Test: `crates/threadlane-coding-agent/src/system_prompt.rs:184+`

**Interfaces:**
- Consumes: `available_tool_names` in `build_system_prompt()`
- Produces: Expanded system prompt guideline statements for `edit_file_hashline`

- [ ] **Step 1: Write failing unit test for system prompt guidelines**

Add `test_hashline_system_prompt_guidelines` to `crates/threadlane-coding-agent/src/system_prompt.rs` inside `mod tests`:

```rust
    #[test]
    fn test_hashline_system_prompt_guidelines() {
        let tools = vec![
            tool("read_file", "Read a file."),
            tool("edit_file_hashline", "Edit file with hashline."),
        ];
        let prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &SystemPromptConfig::default(),
            work_dir: Path::new("/workspace"),
            tools: &tools,
            project_context: &ProjectContext::default(),
            skill_catalog: None,
            agent_catalog: None,
            loaded_extension_count: 0,
        });

        assert!(prompt.contains("Prefer `edit_file_hashline` for high-precision edits using line:hash anchors (e.g. '12:a3f') returned from `read_file`."));
        assert!(prompt.contains("For multi-line code blocks or deletions, use range edits (start_anchor and end_anchor) rather than per-line edits."));
        assert!(prompt.contains("Batch all edits for a file into a single `edit_file_hashline` tool call's edits array."));
        assert!(prompt.contains("If a hashline mismatch occurs, re-read the relevant file range with `read_file` to obtain updated line hashes before retrying."));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p threadlane-coding-agent test_hashline_system_prompt_guidelines`
Expected: FAIL with missing guideline text assertions.

- [ ] **Step 3: Update `build_system_prompt` in `crates/threadlane-coding-agent/src/system_prompt.rs`**

Replace lines 128-130 of `crates/threadlane-coding-agent/src/system_prompt.rs`:

```rust
        if available_tool_names.contains("edit_file_hashline") {
            add_guideline("Prefer `edit_file_hashline` for high-precision edits using line:hash anchors (e.g. '12:a3f') returned from `read_file`.");
            add_guideline("For multi-line code blocks or deletions, use range edits (start_anchor and end_anchor) rather than per-line edits.");
            add_guideline("Batch all edits for a file into a single `edit_file_hashline` tool call's edits array.");
            add_guideline("If a hashline mismatch occurs, re-read the relevant file range with `read_file` to obtain updated line hashes before retrying.");
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p threadlane-coding-agent test_hashline_system_prompt_guidelines`
Expected: PASS

- [ ] **Step 5: Run all `threadlane-coding-agent` tests**

Run: `cargo test -p threadlane-coding-agent`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-coding-agent/src/system_prompt.rs
git commit -m "feat(agent): expand system prompt guidelines for edit_file_hashline"
```

---

### Task 3: Update Repository Documentation & Guidelines in `AGENTS.md`

**Files:**
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: Hashline optimization rules defined in design spec
- Produces: Updated repository guidelines for future agent sessions

- [ ] **Step 1: Check existing `AGENTS.md` wording**

Read the section in `AGENTS.md` referencing `edit_file_hashline`.

- [ ] **Step 2: Update `AGENTS.md` with guidelines**

In `AGENTS.md`, under the guidelines section, update the `edit_file_hashline` bullet to:

```markdown
- Prefer `edit_file_hashline` for high-precision edits using line:hash anchors (e.g. '12:a3f') returned from `read_file` to ensure edit safety and prevent line drift. Use range edits (`start_anchor` to `end_anchor`) for multi-line replacements/deletions, batch multiple edits into a single tool call, and re-read the target range with `read_file` if a hash mismatch occurs.
```

- [ ] **Step 3: Run repository whitespace check**

Run: `git diff --check`
Expected: PASS (no whitespace errors)

- [ ] **Step 4: Run workspace check**

Run: `cargo check -p threadlane`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add AGENTS.md
git commit -m "docs: update AGENTS.md with hashline range edit, batching, and recovery guidelines"
```
