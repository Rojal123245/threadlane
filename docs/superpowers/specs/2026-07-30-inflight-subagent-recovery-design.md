# In-Flight Subagent Recovery Design

## Goal

Make subagent lanes crash-safe while they are running. Persist child operation and tool intent
before execution, checkpoint completed child messages onto the parent `SessionTree`, and recover
interrupted lanes without executing unsafe tools twice.

## Architecture

Reuse the existing session sidecar oplog and `OpRecord` variants. Every subagent uses its existing
lane identity, `subagent-{run_id}:{task_index}`, and writes to the parent session's
`.oplog.jsonl`. No separate subagent journal, session file, or persistence backend is introduced.

The short-lived child `Agent` remains the execution engine. Its lane context carries the parent
session file, captured parent leaf, lane name, run ID, and a synchronous tool-intent recorder.

## Lifecycle

### Start

Before a child model call:

1. Allocate the subagent run ID and lane name.
2. Persist `OperationStarted` for that lane.
3. Persist `TaskAttempt` containing the assigned task.
4. Append a passive `subagent_lane` marker with status `running` to the parent `SessionTree`.
5. Only then call the child model.

A failed append prevents the child from starting.

### Tool execution

The child installs the existing `ToolIntentRecorder` on its `AgentLoop`. After argument
normalization and policy approval, but before `ToolExecutionStart` or executor invocation, it
persists `ToolStarted` under the child lane.

The record retains the existing replay classification:

- `ToolReplaySafety::Safe`: read-only tool may be replayed once during recovery.
- `ToolReplaySafety::Never`: command, write, or otherwise unsafe tool is never replayed
  automatically.

A failed `ToolStarted` append returns a tool error and prevents executor invocation.

### Checkpoints

After each complete child turn, append newly completed non-system messages to the child passive
branch:

- finalized reasoning;
- assistant text and tool calls;
- completed tool results.

Checkpoints occur only at message/turn boundaries. Partial token streams are not persisted.
`SessionTree.active_node_id` remains on the parent branch.

### Finish

Persist `OperationFinished` after the final child checkpoint:

- `Completed` when the child produced a final response;
- `Failed` for provider, persistence, or tool-loop errors;
- `Aborted` for cancellation or an interrupted unsafe tool.

The parent receives only the existing formatted final subagent tool result.

## Recovery

When loading the parent session:

1. Group oplog records by subagent lane.
2. Find `OperationStarted` records without matching `OperationFinished`.
3. Reconstruct each lane from its passive branch and durable tool intents.
4. For an unfinished safe tool with no persisted result, replay it once and append the result to
   that lane.
5. For an unfinished unsafe tool, append one synthetic error result and finish the lane as
   `Aborted`.
6. If no tool is open, resume the child from its latest checkpoint and finish the assigned task.
7. Persist `OperationFinished` before exposing the recovered result to the parent.

Recovery is idempotent. A second restore must not create another tool intent, synthetic result,
checkpoint node, or finished record.

## Data and Interfaces

Add an internal child context:

```rust
struct SubagentLaneContext {
    session_id: String,
    session_file: PathBuf,
    lane_name: String,
    run_id: String,
    parent_leaf_id: Option<String>,
}
```

Extend the existing completed lane metadata with `run_id` and the latest passive branch leaf.
Use the current oplog append/load helpers, `ToolIntentRecorder`,
`classify_tool_replay_safety`, and `reconcile_op_log_recovery`.

No new public frontend API is required.

## Concurrency

Parallel children reserve record sequence numbers and append under the existing lane/session
locking path. Each child mutates only its own passive branch. Parent-tree checkpoint commits remain
serialized by the owning `CodingAgent`, preserving collision-free node IDs and parent active-branch
state.

## Error Handling

- Intent persistence failure prevents the associated model/tool action.
- Checkpoint failure marks the child failed and preserves its prior durable checkpoint.
- Cancellation finishes the active child operation as aborted.
- Safe replay failure marks the recovered lane failed.
- Unsafe interruption is never retried automatically.
- Recovery errors are surfaced through existing `AgentError` and supervisor task status paths.

## Testing

- Start-order test: operation and task records exist before the child model runner is invoked.
- Tool-order test: `ToolStarted` exists before the executor runs; append failure blocks execution.
- Checkpoint test: completed turn messages survive reload without moving the parent active leaf.
- Cancellation test: active child operation finishes as aborted.
- Safe-replay test: interrupted read-only tool executes once and persists its result.
- Unsafe-recovery test: interrupted command/write gets one synthetic error and no execution.
- Idempotency test: running recovery twice creates no duplicate records or branch messages.
- Parallel test: sibling lanes receive distinct IDs and monotonic per-lane sequences.

## Scope Boundary

This slice does not persist partial token deltas, migrate historical disconnected subagent
results, add a lane browser, or automatically retry failed write/command work. Those additions
require separate evidence and designs.
