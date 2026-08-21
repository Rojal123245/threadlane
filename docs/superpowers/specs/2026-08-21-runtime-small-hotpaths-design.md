# Runtime Small Hot Paths Design

## Goal

Remove repeated client construction, path canonicalization, unbounded search work, schema cloning, Git subprocess fan-out, and supervisor polling without weakening intent-first durability or workspace isolation.

## Scope

This change covers findings 12–19 across `threadlane-provider`, `threadlane-tools`, `threadlane-runtime`, `threadlane-session`, `threadlane-mcp`, `threadlane-wasi`, `threadlane-git`, and the GPUI integration build. It adds no dependencies and preserves existing external APIs unless explicitly noted below.

## HTTP Client and Model Cache

`threadlane-provider::openai` will expose one lazily initialized process-wide `reqwest::Client`. `OpenAIClient` instances clone that handle, which preserves reqwest's connection pool across model refreshes, title generation, commit-message generation, and ordinary provider requests.

Successful `/v1/models` results will be cached for five minutes. The cache key will be a standard-library hash of the API key plus account ID so raw credentials are not retained as map keys. Failed requests and built-in fallback lists will not be cached, allowing a later refresh to recover. The cache remains process-local and bounded to the credential identities used during the process lifetime.

## Canonical Workspace Roots

`threadlane-tools` will use a process-wide `OnceLock<Mutex<HashMap<PathBuf, PathBuf>>>` to memoize successful workspace-root canonicalization. Failed canonicalizations are not cached. Existing target and nearest-existing-ancestor canonicalization remains unchanged, preserving symlink escape protection.

When `validate_cwd_in_workspace` receives `None`, it will return the cached canonical root directly instead of canonicalizing it a second time.

## Async Filesystem Boundaries

Blocking filesystem work will move off async workers only where the operation owns all required data and can be transferred safely:

- ACP async discovery will load global and project settings inside `tokio::task::spawn_blocking`.
- Async path-scoped harness helpers that open a short-lived store will perform the complete open/append operation inside `spawn_blocking` when ownership permits.
- Existing ACP workspace reads and writes continue using `tokio::fs`.

`HarnessSupervisor::new`, registry loading, and long-lived mutable `CodingSessionHarness` methods remain synchronous because they are synchronous constructors or borrow live state that cannot safely cross a blocking-task boundary without redesign. They will not receive superficial async wrappers. Any broader conversion requires a measured baseline and a separate design.

## JSONL Durability Classes

JSONL appends remain serialized through the existing process-wide append lock.

- Entries and intent/lifecycle records retain `File::sync_all`.
- `StreamCheckpoint` and low-volume observational provider trace records use `File::sync_data`.
- No timer-based batching is introduced.

The record classification will live beside the record append path and be exhaustively matched so new record variants default to full durability until deliberately classified. A behavioral test will verify which sync policy each representative record selects without relying on timing.

## Bounded Grep Search

The in-process search keeps its existing traversal and glob behavior, with these fixed ceilings:

- Skip files larger than 2 MiB based on metadata.
- Read bytes and skip any file containing a NUL byte.
- Skip invalid UTF-8 files.
- Stop after 1,000 matches or 1 MiB of formatted output, whichever occurs first.
- Append one explicit truncation line when either result cap is reached.

The search will continue skipping `.git`, `target`, and `.threadlane`. Tests will cover oversized files, binary files, match capping, and normal results.

## Shared Tool Definition Slices

The internal `ToolExecutor::tool_definitions` contract will return `Arc<[AgentToolDefinition]>`. Runtime collection and routing will iterate the shared slice and clone definitions only when constructing the final provider request.

MCP will store its discovered definitions as `Arc<[AgentToolDefinition]>` and replace the slice when discovery changes. WASI will cache an `Arc<[AgentToolDefinition]>` on the manager and rebuild it only after registration or explicit reload. Other executors may construct a slice on demand initially; this change specifically removes repeated deep cloning of MCP and WASI JSON schemas.

This is an internal Rust interface migration. Every `ToolExecutor` implementation and dispatcher caller must compile against the new return type in the same change.

## Constant-Spawn Commit Message Diff

The existing staged-diff fast path remains first and returns without index mutation. When there are no staged changes:

1. Run `git add --intent-to-add -- .`.
2. Run one `git diff --` to include tracked and intent-to-add files.
3. Run `git reset --mixed` to remove intent-to-add entries.

Cleanup runs regardless of diff success. Cleanup failure is returned rather than leaving an unexpected index mutation. This path requires an existing `HEAD`; unborn repositories retain the current fallback behavior. `diff_file` will store the `ls-files` result once instead of spawning it twice.

Tests will compare index state before and after success and failure, and verify multiple untracked files appear in one combined diff.

## Event-Driven Harness Watching

`HarnessEventHub` will own a Tokio `Notify` shared by its clones. Publishing an event will notify waiters after the event is committed to the in-memory queue.

`HarnessWatch` will add an async `recv` method that:

1. creates the notification future;
2. polls the subscription;
3. returns immediately when events exist;
4. otherwise awaits notification and repeats.

This ordering prevents a publish between polling and sleeping from being lost. The supervisor harness listener will await `recv` instead of waking every 50 ms. Existing synchronous `poll` remains for callers and tests that need it.

## Error Handling and Security

- Cache lock poisoning degrades to uncached behavior rather than failing requests.
- Credential material is never logged or serialized.
- Workspace validation retains canonical target and ancestor checks.
- Git cleanup errors are surfaced and never silently leave index changes.
- New record variants remain fully durable by default.
- Notification gaps retain the existing `EventError::Gap` behavior.

## Measurement and Verification

Before implementation, run the existing ignored search and relevant runtime performance harnesses. Add deterministic behavioral tests rather than timing assertions.

Required verification:

- Focused tests for provider model caching, canonical-root reuse, bounded search, durability classification, shared schema slices, Git index cleanup, and event-driven watch wakeup.
- Existing `threadlane-mcp`, `threadlane-wasi`, `threadlane-git`, `threadlane-tools`, `threadlane-runtime`, and `threadlane-session` suites.
- `cargo check -p threadlane-gpui`.
- Existing ignored performance harnesses before and after.
- `git diff --check` and final diff review.

The known live-network `threadlane-tools::test_read_file_virtual_schemes_and_urls` fixture may remain environment-dependent; it is not part of these hot-path changes and must be reported separately if it fails again.
