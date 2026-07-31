# Session-Tree Subagent Lanes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist every subagent as a passive sibling branch on its parent's `SessionTree` while keeping child transcript details off the parent active conversation.

**Architecture:** Keep the current child `Agent` for provider and tool execution. Capture one parent leaf per dispatch, return immutable completed-lane transcripts from child tasks, and commit them serially through a new passive `SessionTree` branch API after the parent turn. Existing lifecycle events continue driving supervisor/UI state.

**Tech Stack:** Rust, Tokio, existing `Agent`/`AgentLoop`, JSONL `SessionTree`, `HarnessSupervisor`, Cargo tests.

## Global Constraints

- Parallel subagents must all branch from the same captured parent leaf.
- Child branches must never change `SessionTree.active_node_id`.
- Persist child non-system messages in execution order, including reasoning, assistant tool calls, and tool results.
- Only the existing formatted `subagent` tool result enters the parent active branch.
- Use the existing session JSONL file and session-file lock; add no persistence backend or dependency.
- Child runtime failures still preserve a failed lane transcript.
- A branch persistence failure must surface as a typed input error.
- In-flight crash resume and live partial transcript persistence are outside this slice.
- Use TDD for every behavioral change.

---

### Task 1: Add atomic passive branch persistence

**Files:**
- Modify: `crates/threadlane-agent/src/session_tree.rs`
- Test: `crates/threadlane-agent/src/session_tree.rs`

**Interfaces:**
- Produces:

```rust
pub fn append_passive_branch(
    &mut self,
    parent_leaf_id: Option<&str>,
    messages: Vec<AgentMessage>,
) -> Result<Vec<String>, String>
```

- The method returns created node IDs in message order and leaves `active_node_id` unchanged.

- [ ] **Step 1: Write failing tests**

Add:

```rust
#[test]
fn passive_branch_append_preserves_active_leaf_and_order()
```

Create a persisted tree with an active parent branch, append two child branches from the same
captured leaf, and assert:

```rust
assert_eq!(tree.active_node_id(), Some(parent_active.as_str()));
assert_eq!(tree.get_branch_messages(branch_a.last().map(String::as_str)), expected_a);
assert_eq!(tree.get_branch_messages(branch_b.last().map(String::as_str)), expected_b);
```

Add:

```rust
#[test]
fn passive_branch_append_rolls_back_when_persistence_fails()
```

Point `file_path` at a missing parent directory and assert the method returns `Err`, node count is
unchanged, and the active node is unchanged.

- [ ] **Step 2: Verify RED**

Run:

```bash
rtk cargo test -p threadlane-agent passive_branch_append
```

Expected: compile failure because `append_passive_branch` does not exist.

- [ ] **Step 3: Implement the minimum atomic append**

Validate `parent_leaf_id` when non-empty, pre-allocate collision-free `node_N` IDs, build a linear
node chain, and append the nodes plus unchanged metadata while holding `session_file_lock()`.
Insert into memory only after the file append succeeds, or restore the original vectors on error.
Do not call `add_message_at_leaf`, because that method may advance the active node.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
rtk cargo test -p threadlane-agent passive_branch_append
rtk cargo test -p threadlane-agent session_tree
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/threadlane-agent/src/session_tree.rs
git commit -m "feat: append passive session branches atomically"
```

---

### Task 2: Return connected lane transcripts from subagent execution

**Files:**
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Test: `crates/threadlane-coding-agent/src/coding_agent.rs`

**Interfaces:**
- Consumes: `SessionTree::append_passive_branch`.
- Produces:

```rust
#[derive(Clone, Debug)]
enum SubagentLaneStatus {
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
struct CompletedSubagentLane {
    lane_name: String,
    parent_leaf_id: Option<String>,
    task: String,
    agent: String,
    status: SubagentLaneStatus,
    messages: Vec<AgentMessage>,
    error: Option<String>,
}

```

Keep the existing `AgentRunner` signature unchanged because it is also consumed by brokered
`agent.run`. Change only:

```rust
async fn run_subagents_with_context(
    tasks: Vec<AgentRunTask>,
    parallel: bool,
    context: SubagentRunContext,
) -> Result<(String, Vec<AgentMessage>, Vec<CompletedSubagentLane>), String>
```

The existing runner closure pushes the returned lanes into the completed-lane sink, then returns
the same model-visible JSON `Value` as today.

- [ ] **Step 1: Write the failing transcript test**

Add:

```rust
#[tokio::test]
async fn parallel_subagents_return_sibling_lane_transcripts()
```

Use the existing test observer path with two tasks and one captured parent leaf. Assert both
returned lanes use the same `parent_leaf_id`, have names `subagent-{run_id}:0` and
`subagent-{run_id}:1`, and retain task-index order.

Add a deterministic child test executor path that returns:

```rust
vec![
    AgentMessage::User { content: task.clone() },
    AgentMessage::Assistant {
        content: Some("child result".into()),
        tool_calls: None,
        stop_reason: None,
        deferred_handle: None,
    },
]
```

- [ ] **Step 2: Verify RED**

Run:

```bash
rtk cargo test -p threadlane-coding-agent parallel_subagents_return_sibling_lane_transcripts
```

Expected: compile/test failure because the runner returns JSON without lane transcripts.

- [ ] **Step 3: Implement lane result collection**

Add `parent_leaf_id` to `SubagentRunContext`. Make `run_subagent_task` always snapshot the child's
non-system `AgentState.messages` before returning. On failure, return a failed
`CompletedSubagentLane` alongside the error text instead of discarding collected messages.
Keep provider-model fallback inside the same lane identity.

Update `run_subagents_with_context` to preserve task-index ordering through the existing
`join_all` result order and return `(output, thinking, lanes)`. Represent child completion as:

```rust
struct SubagentTaskOutcome {
    result: Result<SubagentResult, String>,
    lane: CompletedSubagentLane,
}
```

so model/tool errors retain a failed lane instead of losing the transcript. Convert timeout and
semaphore failures into failed lanes in `run_subagents_with_context`, where task metadata is still
available.

- [ ] **Step 4: Preserve the external tool response**

In the existing `AgentRunner` closure, extend the completed-lane sink with `lanes` and continue
returning only:

```rust
serde_json::json!({
    "message": output,
    "output": output,
    "thinking": thinking,
})
```

Do not expose raw lane transcripts in model-visible tool output and do not change
`SubagentToolExecutor`.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
rtk cargo test -p threadlane-coding-agent parallel_subagents_return_sibling_lane_transcripts
rtk cargo test -p threadlane-coding-agent subagent
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-coding-agent/src/coding_agent.rs
git commit -m "feat: retain subagent lane transcripts"
```

---

### Task 3: Commit completed lanes to the parent SessionTree

**Files:**
- Modify: `crates/threadlane-coding-agent/src/coding_agent.rs`
- Test: `crates/threadlane-coding-agent/src/coding_agent.rs`

**Interfaces:**
- Consumes: `CompletedSubagentLane`, `SessionTree::append_passive_branch`.
- Produces:

```rust
fn commit_completed_subagent_lanes(&mut self) -> Result<(), String>
```

- Add two internal shared values captured by the existing runner closure:

```rust
dispatch_parent_leaf: Arc<std::sync::Mutex<Option<String>>>,
completed_subagent_lanes: Arc<std::sync::Mutex<Vec<CompletedSubagentLane>>>,
```

- [ ] **Step 1: Write the failing parent-tree test**

Add:

```rust
#[tokio::test]
async fn completed_subagents_persist_as_passive_sibling_branches()
```

Seed a parent session, capture its active leaf, place two completed lanes in the sink, call
`commit_completed_subagent_lanes`, and assert:

```rust
assert_eq!(coding_agent.session_tree.active_node_id(), Some(parent_leaf.as_str()));
```

Find both `subagent_lane` custom markers in `session_tree.nodes`, verify their parent IDs equal the
captured parent leaf, and verify reloading the session file retains both complete branches.

Add:

```rust
#[test]
fn child_messages_do_not_enter_the_parent_active_branch()
```

Assert the parent active branch contains the formatted outer subagent tool result but none of the
child user, reasoning, or tool messages.

- [ ] **Step 2: Verify RED**

Run:

```bash
rtk cargo test -p threadlane-coding-agent completed_subagents_persist_as_passive_sibling_branches
```

Expected: compile failure because the lane sink and commit method do not exist.

- [ ] **Step 3: Capture one parent leaf per parent turn**

Immediately after:

```rust
let node_id = self.session_tree.add_message(msg.clone());
```

store `Some(node_id)` in `dispatch_parent_leaf`. Clear it after the parent agent call returns.
Every runner invocation during that turn clones the same captured value into
`SubagentRunContext.parent_leaf_id`.

- [ ] **Step 4: Commit lanes after the parent agent returns**

Drain the sink in task-index order. For each lane, prepend:

```rust
AgentMessage::Custom {
    custom_type: "subagent_lane".into(),
    payload: serde_json::json!({
        "lane": lane.lane_name,
        "agent": lane.agent,
        "task": lane.task,
        "status": match lane.status {
            SubagentLaneStatus::Completed => "completed",
            SubagentLaneStatus::Failed => "failed",
        },
        "error": lane.error,
    }),
}
```

Append the marker and lane messages with `append_passive_branch`. On any error, send
`AgentEvent::AgentError` and return `Some(Err(error))` from input handling. Do not append a partial
branch.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
rtk cargo test -p threadlane-coding-agent completed_subagents_persist_as_passive_sibling_branches
rtk cargo test -p threadlane-coding-agent child_messages_do_not_enter_the_parent_active_branch
rtk cargo test -p threadlane-coding-agent subagent
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/threadlane-coding-agent/src/coding_agent.rs
git commit -m "feat: persist subagents on parent session tree"
```

---

### Task 4: Wire supervisor lane lineage and cancellation

**Files:**
- Modify: `crates/threadlane-coding-agent/src/supervisor.rs`
- Test: `crates/threadlane-coding-agent/src/supervisor.rs`

**Interfaces:**
- Consumes existing `AgentEvent::SubagentQueued { run_id, task_index, .. }`.
- Produces no new public API; reuses:

```rust
HarnessSupervisor::get_or_create_sub_lane(
    session_id,
    &format!("subagent-{run_id}:{task_index}"),
    "main",
)
```

- [ ] **Step 1: Write the failing lineage test**

Add:

```rust
#[test]
fn subagent_events_create_sibling_supervisor_lanes()
```

Observe two `SubagentQueued` events for one session and assert both lanes exist, both have
`parent_lane == Some("main")`, and cancelling `main` marks both child lanes cancelling.

- [ ] **Step 2: Verify RED**

Run:

```bash
rtk cargo test -p threadlane-coding-agent subagent_events_create_sibling_supervisor_lanes
```

Expected: FAIL because event observation currently creates task records but not lanes.

- [ ] **Step 3: Wire lane creation once**

In the shared supervisor event observation path, create the sub-lane when handling
`SubagentQueued`. Reuse the event-derived lane name; do not create another task runtime or session
file. Keep existing `TaskRecord` creation for UI presentation.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
rtk cargo test -p threadlane-coding-agent subagent_events_create_sibling_supervisor_lanes
rtk cargo test -p threadlane-coding-agent supervisor::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/threadlane-coding-agent/src/supervisor.rs
git commit -m "feat: track subagent session lanes"
```

---

### Task 5: Final validation and durable convention

**Files:**
- Modify: `AGENTS.md`
- No generated files

**Interfaces:**
- Document that model subagents use child `Agent`s only as execution engines; their durable
  history belongs to passive branches of the parent `SessionTree`.

- [ ] **Step 1: Run complete validation**

```bash
rtk cargo test -p threadlane-agent
rtk cargo test -p threadlane-coding-agent supervisor::tests
rtk cargo test -p threadlane-coding-agent subagent
rtk cargo check -p threadlane
rtk git diff --check
```

Expected: all focused tests and checks pass. Record the existing sandbox-only network-test
failures if the complete coding-agent suite is also run.

- [ ] **Step 2: Review the invariants**

Confirm from the diff and tests:

- every parallel child uses the same captured parent leaf;
- passive branches preserve the active parent node;
- child system prompts are not persisted;
- reasoning and tool results are retained on child branches;
- raw child messages do not enter model-visible parent output;
- no separate subagent session file or supervisor runtime is created;
- persistence errors become typed failures.

- [ ] **Step 3: Document the convention**

Add under `Background Tasks and Capabilities`:

```markdown
- Model subagents execute with short-lived child `Agent`s but persist as passive sibling branches
  on the parent `SessionTree`; only the formatted final tool result enters the parent active branch.
```

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md
git commit -m "docs: record parent-tree subagent lanes"
```
