# Harness Activity Restoration Design

## Goal

Restore the latest 20 harness/subagent activity rows after reopening a session, including completed, cancelled, failed, and unfinished work.

## Design

Use the session's existing `*.oplog.jsonl` as the only durable source. During session restoration, reconstruct activity summaries by grouping subagent records by their durable `run_id`. `TaskAttempt` supplies the task identity; the persisted operation terminal record and recovery state supply the final status and detail. Unfinished records remain visible as unresolved activity until the normal recovery path updates them.

The reconstructed activities seed `ChatData.harness_activities` before the first chat draw. Existing live `AgentEvent` reduction continues to update matching keys, so restart recovery and foreground events share one presentation model. Sort by the latest durable sequence/timestamp and retain only the newest 20 rows per session.

Malformed or partially written oplog lines are ignored using the existing tolerant oplog loader. A missing oplog produces no restored activities and does not affect session startup.

## Scope

- Add a small pure reconstruction helper around existing `OpRecord` data.
- Invoke it from the existing session restore path.
- Preserve the current in-memory activity reducer and rail rendering.
- Do not add a second persistence file or change the oplog schema.

## Verification

Add focused tests covering:

1. completed activity reconstruction;
2. cancelled/aborted terminal reconstruction;
3. unfinished activity reconstruction;
4. newest-first ordering and the 20-row cap;
5. malformed or empty records producing safe results.

