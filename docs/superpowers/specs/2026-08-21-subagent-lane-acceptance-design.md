# Subagent Lane Acceptance Design

## Problem

A parent agent invokes the `subagent` tool while its foreground operation remains open on the harness `main` lane. The dispatch path correctly calls `CodingSessionHarness::start_subagent_lane`, which creates a dedicated child lane, starts its operation, commits the child prompt, and records the child lifecycle. However, `run_subagent_task` subsequently calls `CodingSessionHarness::begin_run_text`. That API always accepts a new foreground prompt on `main`, so the runtime rejects it with `Invalid("lane main is busy")`.

Changing the requested agent role does not affect lane selection, so repeated dispatches fail identically. The aggregate tool result also currently remains successful when every child result is a failure, causing the UI to show a misleading success indicator.

## Goals

- Execute each new subagent using the dedicated durable operation created for it.
- Preserve child lane identity, lineage, checkpointing, recovery, cancellation, and lifecycle records in the parent session JSONL.
- Prevent subagent startup from attempting a second operation on `main`.
- Return a failed parent tool result when every child fails.
- Keep mixed-outcome batches successful so useful child outputs remain available, with explicit status and error information for every child.
- Add regression coverage for lane acceptance and aggregate failure semantics.

## Non-goals

- Moving child runs into separate session files.
- Changing concurrency limits, scheduling, role discovery, model routing, or timeout behavior.
- Redesigning the subagent result format beyond the minimum needed to preserve explicit per-child outcomes.
- Changing the rule that a mixed-success batch is a successful tool call.

## Architecture

`start_subagent_lane` remains the single authority for establishing a new child run. It will continue to:

1. Generate collision-resistant child run and lane identifiers.
2. Start the operation on the generated child lane.
3. Commit the user task entry on that lane.
4. Commit the assistant attempt and subagent lifecycle records.
5. Drive all pending effects to durable completion.

After those records are committed, it will produce the proof needed to execute that existing operation: a validated `AcceptedRun` associated with the child lane and child run. The dispatch path will carry this accepted token alongside `SubagentLaneIdentity` into `run_subagent_task`.

`run_subagent_task` will call `AgentRuntime::run_accepted` with the supplied child token. It will not call `begin_run_text`, create another run identifier, append another prompt, or target `main`.

The conceptual flow is:

```text
parent operation on main
  -> subagent tool dispatch
     -> start_subagent_lane
        -> open generated child lane
        -> commit child prompt and assistant attempt
        -> return child identity plus AcceptedRun
     -> run_subagent_task
        -> validate/use existing AcceptedRun
        -> execute provider turn on generated child lane
```

The accepted token must contain:

- the parent session ID;
- the generated child run ID;
- the generated child lane name;
- the child prompt entry ID;
- the child assistant result entry ID; and
- the highest sequence committed through child acceptance.

The token is derived from records actually committed by `start_subagent_lane`, rather than independently generated values. Existing harness validation remains the boundary that checks the token against durable state before provider execution.

## Recovery

Interrupted-subagent recovery already has an existing child lane and run identity. It must use that existing operation and its durable checkpoint without accepting a new foreground run. The implementation must preserve the current recovery prompt and resumed message behavior while ensuring recovery never routes through `main`.

No new session file or parallel state path is introduced. The canonical parent JSONL remains the source of child lineage and progress.

## Batch Result Semantics

Each child retains a structured outcome with its status, output, messages, and error.

- If every child succeeds, the `subagent` tool succeeds and returns all child results.
- If at least one child succeeds and at least one child fails, the tool succeeds and returns all per-child outcomes. The parent can use successful output while seeing each failed child explicitly.
- If every child fails, the tool returns an error. The error must retain enough per-child context to diagnose the failures rather than collapsing them into a generic message.
- Infrastructure errors that prevent result aggregation continue to fail immediately.

This makes the tool-level success indicator describe whether the dispatch produced any usable child result.

## Error Handling

Failure to create or validate a child accepted token is a child startup failure and must include the generated child identity when available. A failed child must still be finalized through the existing durable lifecycle path so open operations do not leak.

The implementation must not retry a `lane main is busy` failure by changing roles or generating another foreground run. Identifier collision retries remain limited to the existing collision-handling behavior in `start_subagent_lane`.

## Tests

Implementation follows red-green-refactor test-driven development.

Regression coverage will verify:

1. With a parent operation open on `main`, creating and accepting a child succeeds on its generated subagent lane.
2. The returned `AcceptedRun` references the existing child run, lane, prompt entry, assistant entry, and committed sequence.
3. Child acceptance does not create an additional operation or prompt on `main`.
4. The child runtime consumes the existing accepted token instead of invoking foreground acceptance.
5. An all-failed batch returns a tool error containing per-child failure context.
6. A mixed-success batch remains successful and includes explicit successful and failed child statuses.
7. Existing parallel, sequential, recovery, cancellation, checkpoint, and durable lifecycle tests continue to pass.

Focused verification will run the relevant `threadlane-session` tests, followed by:

```bash
cargo check -p threadlane-gpui
git diff --check
```

Broader workspace tests will be run if the focused changes expose cross-crate effects.

## Alternatives Rejected

### Add `begin_run_text_on_lane`

The child operation and prompt already exist by the time `run_subagent_task` starts. A second begin API would duplicate lifecycle ownership or encounter the same busy-lane protection on the child lane.

### Give each child a separate session file

Separate files would avoid contention on a file-local `main` lane, but would fragment lineage, recovery, cancellation, and activity projection across stores. The harness already supports multiple lanes in one canonical session, so separate stores add coordination without serving a requirement.

### Treat role names as lane selectors

Roles define subagent configuration, not durable execution identity. Coupling them to lanes would not address the duplicate foreground acceptance and would make repeated use of the same role unsafe.
