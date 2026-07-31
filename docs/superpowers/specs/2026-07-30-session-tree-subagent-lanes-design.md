# Session-Tree Subagent Lanes Design

## Decision

Subagent dispatch will continue using a short-lived child `Agent` as its model/tool execution
engine, but the child will no longer be a disconnected history. Every dispatch captures the
parent `SessionTree` leaf once. Parallel subagents become sibling passive branches anchored at
that leaf, and each branch stores its lane marker, assigned task, reasoning, assistant messages,
tool calls, and tool results.

The parent active branch remains unchanged by child transcript persistence. The existing
`subagent` tool result remains the only child content added to the parent conversation.

This is worth implementing because it gives one durable source of truth for inspection,
navigation, and later recovery work while retaining the existing provider-neutral child agent
loop.

## Alternatives Considered

### Recommended: completion-time passive branch commit

Capture the parent leaf before the parent model turn, retain each child `AgentState.messages`,
then commit successful or failed child transcripts to the parent tree after the subagent tool
finishes. This requires a small passive-branch API and avoids changing the public ownership model
of `CodingAgent.session_tree`.

Trade-off: a process crash during a running child can lose that child's partial transcript.
Durable in-flight subagent checkpoints and resume should be a separate recovery slice.

### Rejected: make the entire parent tree shared mutable state

Changing `CodingAgent.session_tree` to `Arc<Mutex<SessionTree>>` would allow live child writes, but
it would spread locking through the application, supervisor, command handling, tests, and UI.
It also creates lock-order risk around provider callbacks and file persistence.

### Rejected: one session file per subagent

Separate files preserve child history but do not create lanes on the parent tree. Navigation,
retention, and recovery would still need a second registry and cross-file linkage.

## Data Model

Add a transient `CompletedSubagentLane` value:

```rust
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

`lane_name` uses the existing stable event identity:
`subagent-{run_id}:{task_index}`.

The branch begins with an `AgentMessage::Custom` marker:

```json
{
  "custom_type": "subagent_lane",
  "payload": {
    "lane": "subagent-7:0",
    "agent": "reviewer",
    "task": "Review the patch",
    "status": "completed",
    "error": null
  }
}
```

The marker is followed by the child's non-system messages in execution order. System prompts
remain child runtime configuration and are not duplicated into the parent session history.

## Data Flow

1. `CodingAgent::handle_input_with_images` appends the parent user message and captures its node
   ID as the dispatch anchor.
2. The anchor is installed in the existing `AgentRunner` context for the duration of that parent
   turn.
3. `run_subagent_task` runs the child exactly as today and returns both its display result and a
   `CompletedSubagentLane` containing the full non-system transcript.
4. Parallel results retain task-index order regardless of completion order.
5. After the parent agent turn completes, `CodingAgent` drains completed lanes and appends each
   branch passively from the captured anchor.
6. Passive branch writes never change `SessionTree.active_node_id`.
7. Existing formatted subagent output remains the parent tool result. No child reasoning or tool
   message is copied onto the parent active branch.
8. Supervisor event handling creates the matching `Lane` lineage for observability and
   cancellation, using `main` as the parent lane.

## Persistence and Concurrency

`SessionTree::append_passive_branch` performs one branch commit while holding the existing session
file lock. It pre-allocates all node IDs, appends nodes in order, writes metadata with the original
active node, and rolls back in-memory insertion if persistence fails.

Parallel child agents never mutate `SessionTree` directly. They return immutable lane values.
The owning `CodingAgent` commits them serially after the parent model/tool loop returns, avoiding
shared-tree locks and node-ID races.

## Error Handling

- Child model/tool failure still produces a failed lane marker and preserves any transcript
  collected before the failure.
- A passive branch persistence failure emits `AgentEvent::AgentError` and returns a typed error
  from input handling; it must not silently claim the lane was saved.
- An empty or missing parent leaf is allowed only for a draft/root session and creates a passive
  root branch.
- A missing non-empty parent leaf is rejected without mutating the tree.
- Supervisor lane creation remains observational; failure to update UI bookkeeping does not
  duplicate or replace session persistence.

## Testing

- `SessionTree` tests prove passive branch order, persisted reload, unchanged active node, and
  rollback on write failure.
- Coding-agent tests prove parallel subagents share one captured parent leaf, retain full child
  tool history, and leave the parent active branch free of child internals.
- Failure tests prove a child error still creates a failed lane and a persistence error becomes a
  typed input error.
- Supervisor tests prove queued subagent events create sibling lanes under `main` and cancellation
  still targets the hierarchy.

## Scope Boundary

This slice does not resume a child agent after a process crash, stream partial child transcript
nodes while the child is running, or add new frontend views. The persisted branches and existing
subagent lifecycle events provide the data needed for those later slices.
