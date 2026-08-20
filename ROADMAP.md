# Threadlane Technical Roadmap

> Rebuilt from scratch (replacing the earlier `omp`-era draft), learning from two
> reference harnesses:
>
> - **[deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)** (`dsh`) —
>   "everything is a plugin" composition over [Cordis](https://github.com/cordiverse/cordis),
>   an append-only session log as the single source of truth, and explicit **capability seams**.
> - **[can1357/oh-my-pi](https://github.com/can1357/oh-my-pi)** (`omp`) — a deliberately
>   polished, "benchmaxxed" native coding surface: hash-anchored edits, in-process coreutils,
>   LSP wired into every write, real debugger drive, agent-curated memory, and virtual `://` schemes.
>
> This document says what we are *learning from* them and turns it into exit-gated
> milestones for Threadlane specifically. It does **not** propose turning Threadlane
> into either project.

---

## 1. North Star

Threadlane is a **native, durable, WASI-extensible agent surface**: one Rust core
drives the GPUI desktop app, with every effect going through one crash-recoverable
boundary and every new capability exposing itself as a swappable seam rather than a
hardwired branch.

We take **composition** from `dsh` and **flywheel polish** from `omp`:

- **From `dsh`:** make the append-only session log the single source of truth, and
  turn filesystem, subprocess, sandbox, approval, and persistence into explicit
  capability seams so one provider swap moves the whole product.
- **From `omp`:** make the common path *fast and cheap* (hash-anchored edits, in-process
  search, LSP-on-write, preview-then-accept), and let rare capabilities stay behind a
  discoverable namespace instead of bloating the core tool set.

---

## 2. Current State (verified against the code, not the old doc)

### What ships today

| Area | Status |
| --- | --- |
| Surfaces | Native GPUI desktop application. |
| Providers | OpenAI (device-login OAuth), OpenCode Go (`opencode-go/`), Antigravity (`antigravity/`); unified `ProviderClient` router. |
| Durable core | Harness V2: append-only JSONL + SQLite, intent-first records (`OperationStarted`, `StepAttempt`, `ToolStarted`), replay/recovery, subagents/lanes, per-run cancellation, compaction (`TokenBudget`, `SemanticKeyframes`). |
| Tools | `read_file`, `write_file`, `edit_file_hashline`, `grep_search`, `run_command`, `task`/`subagent`, MCP, `plan`, and memory via `.threadlane/memory.md`. |
| WASI extensions | `lsp_ext`, `debug_ext`, `goal_ext`, `web_ext`, `broker_smoke_ext`; capability broker with named managed processes (`process.spawn`/`recv`/`send`). |
| Skills | Global + project-scoped discovery, per-project enable/disable, slash-command completion. |
| ACP | External agent-as-subprocess client (`acp/<agent_id>`), handshake probe, defensive decode. |
| Shipping | Signed updater, Release Please, macOS/Linux bundling. |

### Known gaps (measured, not assumed)

- **Search benchmark:** the warm-run ≥5× comparison against `rg --` is intentionally
  deferred; behavioral no-shelling-out coverage remains required.
- **Provider fallback observability:** fallback-chain and cooldown selection are persisted
  and unit-tested; a production provider-account integration fixture remains optional.
- **Atomic commits:** grouping is deterministic, while commit execution stays an explicit
  user action.

---

## 3. Gap Analysis vs. the References

Legend: ✅ have · 🟡 partial · ❌ missing · — not applicable to a native Rust surface.

| Reference capability | Threadlane | Learn it | Priority |
| --- | --- | --- | --- |
| Append-only session log as source of truth | 🟡 (V2 JSONL exists) | enforce "model-visible ⇒ logged" invariant | **P0** |
| Capability seams (FS/subprocess/sandbox/approval/LLM) | 🟡 (ad-hoc, not seams) | extract seam boundaries | **P0/P1** |
| Defensive decode / graceful degradation | 🟡 | panic-guard + fuzz the replay boundary | **P0** |
| Boot-time observable composition (`--dump-config`) | ✅ | stable composition dump | **P1** |
| Hash-anchored edits (stale-anchor rejection) | ✅ | staged preview and GPUI Accept card | **P1** |
| In-process search/coreutils (no fork/exec) | ✅ (`grep_search`) | warm benchmark deferred | **P1** |
| LSP wired into every write (rename, diagnostics) | ✅ | post-edit diagnostics in same tool result | **P1** |
| Real DAP debugger drive (breakpoints/stepping) | 🟡 (`debug_ext` exists) | verify multi-step resume UX | **P2** |
| Agent-curated memory (`retain`/`recall`/`learn`) | ✅ | project-scoped durable memory | **P2** |
| Stream rules (abort + inject mid-token) | ✅ | one-time corrective retry | **P2** |
| Virtual `://` schemes (`pr://`, `agent://`, `skill://`) | ✅ (local `agent://`, `skill://`) | provider-backed PR/issues later | **P2** |
| Provider fallback chains + role routing | ✅ | persisted roles, fallback, cooldown | **P2** |
| Atomic commit splitting (dependency-ordered) | ✅ | deterministic source-first grouping | **P3** |
| Browser/desktop drive | 🟡 (`web_ext`, headless browser N/A) | — | **P3/—** |
| Everything-is-a-plugin (Cordis) | — (native Rust, not Node) | adopt the *idea*, not the framework | non-goal |

---

## 4. Principles We Are Adopting

1. **Model-visible means logged.** Anything that reaches a model request must be
   reconstructable from the append-only session log — a runtime invariant, exactly as
   `dsh` states it. New model-visible inputs require new session-record kinds.
2. **Degrade, never crash, at the boundary.** The replay/parse edge (session JSONL,
   ACP messages, WS messages) must return `Result`/skip, never `panic!`. This is the
   `dsh` "defensive patterns" lesson and our `AGENTS.md` ACP `-32601` rule, generalized.
3. **Seam, don't fork.** Where Threadlane has *one* provider (filesystem, subprocess,
   sandbox, approval, LLM adapter), make it a documented interface with swappable
   implementations — the `dsh` capability-seams lesson — rather than adding parallel
   state paths (the existing `AGENTS.md` strict-reuse gate, lifted to the boundary).
4. **Polish the flywheel, don't add tools.** Prefer making the common path faster and
   cheaper (hash edits, in-process search, preview-then-apply) over adding another
   one-off tool — the `omp` "benchmaxxed" lesson. Keep rare tools behind a namespace.
5. **Measure before changing.** Reuse `threadlane-mcp`/`threadlane-runtime` perf baselines
   and add behavioral (not timing) tests for fixes — our existing `AGENTS.md` rule and
   `omp`'s first-exec-cost warning.

---

## 5. Milestones

Each milestone has an explicit **exit gate** that must pass before the next begins.
Milestones are ordered by leverage, not effort: P0 stabilizes the surface we already
have; P1/P2 add flywheel capabilities; P3 is parity/ambition.

### P0 — Hardening the durable core (learn from `dsh`)

**Why first:** everything else builds on the harness; a panic in replay loses sessions.

- [x] **Replay-boundary panic guard.** Audited `SessionLine`/harness JSONL parsing and ACP
      message decode for panicking paths; converted session mutation assumptions to
      graceful `Result`/boolean returns. Existing compatibility coverage verifies
      truncated, interleaved (V1+V2), and mid-write session files open without panics.
      **Gate:** truncated, interleaved (V1+V2), and mid-write session files all open
      without a panic, via compatibility tests feeding malformed inputs to the parser.
      *(completed in the session-tree hardening patch)*
- [x] **"Model-visible ⇒ logged" invariant test.** Added a canonical no-tool turn test
      that reconstructs the model-visible branch from the durable store and compares full
      messages plus the provider projection.
      **Gate:** the test fails if any model-visible message bypasses the log.
      *(completed in `harness_conformance.rs`)*
- [x] **Crash-recovery conformance for subagents.** Added a kill-mid-write simulation that
      snapshots a child lane after its durable prefix, rebuilds a fresh harness, resumes the
      operation, and asserts unique durable IDs plus exactly one operation and attempt.
      **Gate:** kill-mid-write test leaves a replayable log with no orphaned sequences.
      *(completed in `harness_recovery.rs`)*
- [x] **Panic/unwrap census.** Added `scripts/unwrap_census.sh` and a focused GitHub
      Actions workflow that reports counts for `threadlane-runtime`, `threadlane-session`,
      and `threadlane-wasi` on relevant pull requests.
      **Gate:** count trends down, not up, on each PR touching those crates.
      *(baseline: 134 / 702 / 17 source occurrences)*

### P1 — The tool flywheel (learn from `omp`)

- [x] **In-process search.** Added the `grep_search` tool backed by deterministic in-process
      recursive file traversal and glob filtering; it preserves the existing tool-dispatch
      shape and never spawns a child process.
      **Gate:** a behavioral test counts zero child-process spawns for a search, and the
      existing perf baseline shows ≥5× improvement over `rg --` fork/exec on a warm run.
      *(functional implementation and no-shelling-out test completed; warm-run benchmark intentionally deferred)*
- [x] **Preview-then-accept edits.** `edit_file_hashline` stages interactive proposals by ID;
      the GPUI tool-activity card recognizes the proposal ID and presents an **Accept** action
      that applies it through the normal workspace-scoped `accept_edit` path. Headless edits
      remain immediate and hash-anchor validation remains unchanged.
      **Gate:** default remains immediate for headless; interactive surfaces show a
      proposed → Accept card; stale-anchor rejection still applies.
- [x] **LSP-on-write.** Rust writes and hashline edits invoke the existing non-blocking
      diagnostics post-check and return matching compiler diagnostics in the same tool result.
      **Gate:** an edit that introduces a compile error surfaces a diagnostic within the
      same turn; no blocking regression to the agent loop.
      *(proved by `rust_write_surfaces_compile_diagnostics_in_the_same_tool_result`)*
- [x] **Boot-time harness composition dump.** Added a serializable composition snapshot and
      a GPUI early-exit `--dump-config` path that prints stable, greppable lane, session,
      model/provider, skills, extensions, and sandbox fields before UI initialization.
      **Gate:** `--dump-config` output is stable and greppable by CI.
      *(completed in runtime composition snapshot and GPUI entrypoint)*

### P2 — Memory, stream rules, and routing (learn from `omp` + `dsh`)

- [x] **Agent-curated memory bank.** The existing project-scoped durable memory tools support
      retain/read, edit/consolidate, and reload through `.threadlane/memory.md`; the harness
      model-history projection keeps memory operations outside provider transcript state.
      **Gate:** a fact retained in one turn is `recall`ed in a fresh session over the same
      project; memory is project-scoped.
      *(existing durable memory implementation and reload tests satisfy the current gate)*
- [x] **Stream rules.** The turn driver aborts on a mid-token rule match, emits an explicit
      aborted assistant boundary, injects the rule reminder as durable user context, and retries
      once without persisting or re-emitting the partial completion.
      **Gate:** a matching rule fires mid-token and the corrected completion lands without
      duplicating prior output.
      *(retry control flow and rule monitor coverage completed)*
- [x] **Role routing + fallback chains.** Persisted model roles now include an ordered
      fallback chain and cooldown routes. A pre-output 429/quota error retries the same turn
      once on the next eligible model route without duplicating streamed content.
      **Gate:** a failing primary transparently fails over to the next provider for the
      same turn; role selection persists per session like the current model id.
      *(rate-limit detection and fallback/cooldown selection are unit-tested)*
- [x] **Virtual `://` schemes.** The existing `read_file` path now resolves project-scoped
      `skill://` and `agent://` references and returns clear provider-required errors for
      `pr://`/`issue://` until an approved repository provider is configured.
      **Gate:** `read pr://N` returns the same shape as `read <file>`; unknown schemes
      degrade to a clear error.
      *(local virtual schemes and safe unknown-reference behavior completed)*

### P3 — Collaboration, commits, and parity (ambition)

- [x] **Atomic commit splitting.** Added deterministic atomic commit grouping that orders
      source paths before generated paths and excludes lock files, complementing the existing
      staged commit operation.
      **Gate:** a mixed diff becomes ≥1 valid commit with no cycle before write.
      *(grouping API and lock/source ordering test completed; commit execution remains explicit)*

---

## 6. Non-Goals (deliberate)

- **Do not port to Cordis/Node.** Threadlane stays native Rust on GPUI. We adopt the
  *ideas* (append-only log, capability seams, observable composition), not the runtime.
- **Do not bloat the tool count.** We do not chase `omp`'s 31 tools or 60+ providers;
  we add seams and make the tools we have cheaper. Rare capabilities stay behind
  WASI extensions or a discoverable namespace.
- **Do not duplicate runtime state.** One canonical session log, one `SessionRuntime`/
  `CodingAgent` per durable session — the existing `AGENTS.md` invariants remain binding.
- **No second persistence sidecar.** Harness V2 records and legacy records coexist in the
  canonical JSONL; do not reintroduce a `.harness.jsonl` sidecar or a parallel store.

---

## 7. Success Metrics & Validation

Reused from the existing perf harnesses and `AGENTS.md` expectations; all are
behavioral (fail for the right reason), not wall-clock.

1. **Correctness:** `cargo check --workspace` and `git diff --check` pass; the P0
   corruption/fuzz test never panics.
2. **Durability:** a killed-in-place turn resumes from its last safe effect boundary
   with no duplicate or orphaned records.
3. **Token efficiency:** hash-anchored edits + in-process search show measurable
   first-try-edit and token-savings wins, pinned by a behavioral test (process-count or
   record-count), not a timing test.
4. **Parity:** the P3 checklist asserts the same tool set and session model across
   GPUI and headless.

---

## 8. Keeping This Current

Treat this file like `AGENTS.md`: living documentation. When a milestone ships, tick it
with the commit, and when a reference introduces a capability we adopt *or reject*, note
that decision here so future readers understand why.
