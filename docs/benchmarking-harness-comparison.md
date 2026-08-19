# Threadlane vs. DeepSeek Harness Benchmark Protocol

## Purpose

Measure Threadlane and DeepSeek Harness with the same provider/model configuration before attributing differences in latency or task quality to either harness.

## Controls

Run each task from a clean workspace copy and a fresh session unless the case explicitly tests long-session continuation.

Keep these identical across systems:

- Provider, model identifier, and account/credential route.
- Reasoning effort, output-token cap, temperature, and fallback policy.
- Repository revision and workspace path policy.
- Tool availability, permissions, and MCP configuration.
- Network conditions as far as practical; record warm versus cold provider connection/cache state.

Never record credentials, request headers, cookies, or raw provider bodies.

## Task set

Use at least three repetitions of each task and score the final artifacts independently.

| ID | Scenario | Primary measures |
| --- | --- | --- |
| T1 | Direct repository question requiring no tools | time to first visible token, answer correctness |
| T2 | Single-file bug fix with focused test | valid-first-tool-call rate, test pass, elapsed time |
| T3 | Multi-file change with verification | completion rate, tool count, total time |
| T4 | Repository exploration then implementation | discovery accuracy, unnecessary reads, elapsed time |
| T5 | Large command/search output | model-context size, follow-up correctness, latency |
| T6 | Multiple independent read-only tools | parallelism and tool completion latency |
| T7 | Interrupted provider/tool turn and resume | durable replay fidelity and recovery correctness |
| T8 | Long session with a requirement buried mid-history | retained-constraint accuracy and context size |
| T9 | Session reopen/switch with a large transcript | cold/open latency and visible-history correctness |
| T10 | Rate limit/fallback/retry fixture | retry timing, duplicate output, user-visible status |

## Required event timeline

For every Threadlane provider attempt, record a correlation ID and monotonic timestamps for:

```text
prompt accepted
→ durable prompt intent
→ provider request start
→ first provider event
→ first agent event
→ first UI drain / first visible token
→ tool preflight / durable intent / execution start / finish
→ provider terminal event
→ durable reconciliation complete
→ input available
```

Also record:

- input/output/cache token usage when supplied by the provider;
- message count and estimated context size;
- tool schema and system-prompt fingerprints;
- loaded skills/capabilities;
- retry/fallback/compaction decision;
- stream queue high-water mark and blocked-send duration;
- persistence append/reduce timings and file size;
- GPUI drain event count and duration.

Use the equivalent observable timestamps available in DeepSeek Harness. If an event is unavailable, mark it `not_available`; do not infer it from unrelated timestamps.

## Scorecard

Report median and P95 per task/system for:

- prompt-to-request-start;
- request-start-to-first-provider-token;
- first-provider-token-to-first-visible-token;
- end-to-end completion;
- tool latency by phase;
- final durable reconciliation;
- session-open/switch latency.

For quality, independently score:

- task completion and test result;
- valid first tool call;
- retained user constraints;
- false completion;
- recovery/retry correctness;
- clarity of visible lifecycle state.

## Acceptance criteria for a Threadlane optimization

A change must:

1. Preserve targeted behavior/recovery tests.
2. Improve a measured P50 or P95 metric on the relevant benchmark.
3. Not reduce task-success or recovery scores.
4. Preserve durable ordering and avoid exposing credentials or unbounded raw request data.
