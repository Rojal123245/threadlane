# Virtualized Session History Design

## Goal

Keep very long chat sessions smooth by rendering only visible transcript rows and reading older JSONL history from disk on demand without opening or reducing the complete session store.

## Architecture

The chat view will use GPUI's existing variable-height `list` element with a `ListState`. A cached row model will preserve the existing grouping of consecutive tool-only messages, while the list renders only its visible range. Tail following remains enabled during generation; prepending an older page preserves the visible anchor, and streaming invalidates only the final row's measured height.

The runtime will expose a read-only transcript page API. It seeks backward from an opaque byte cursor in bounded chunks, ignores a torn final JSONL line, recognizes current `Entry` and legacy session-node lines, and returns main-lane messages in chronological order. It does not instantiate `JsonlStore`, validate unrelated durable records, or run `ReductionContext` because the UI transcript is defined as chronological main-lane entries rather than model-context reduction.

GPUI will initially request the newest 40 projected messages and request older pages near the top. Full trajectory, diagnostics, metrics, token usage, and plan projection remain asynchronous and independent from first transcript paint. A persistent sidecar index is explicitly out of scope; the byte cursor is sufficient for sequential upward history loading.

## Data Flow

1. Initial history load calls `read_transcript_page(path, None, limit)` on a background executor.
2. The reader scans backward from EOF and returns raw messages plus `next_cursor`.
3. GPUI projects that page to `ChatMessageInfo`, prefixes page-local IDs with the cursor boundary, and installs it.
4. Scrolling near the first visible row requests another page using `next_cursor` and prepends it.
5. `ListState` updates its item count and keeps the previous top row anchored.
6. New live messages append normally; only visible rows are materialized by GPUI.

## Correctness and Failure Handling

- A final non-newline-terminated fragment is treated as a torn append and skipped.
- Malformed complete lines encountered inside the requested page return an error.
- A cursor beyond the current file length is clamped, allowing a stale request after an append.
- A shrunk or replaced file invalidates the cursor and restarts from EOF.
- Page boundaries scan back to a user message after the minimum page size so reasoning and tool activity remain with their turn.
- The canonical JSONL stays the only source of truth; cursors and rendered-row caches are disposable.

## Tests

- Runtime tests cover newest-page order, older-page continuation, bounded backward scanning, legacy nodes, and torn tails.
- GPUI state tests cover page application and unique IDs.
- GPUI view tests cover row grouping and list-state changes for append, prepend, session reset, and streaming remeasurement.
- Run focused runtime/GPUI tests, `cargo check -p threadlane-gpui`, `cargo test --workspace`, and `git diff --check`.

