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
drives a GPUI desktop app, a Ratatui TUI, and a headless CLI, with every effect going
through one crash-recoverable boundary and every new capability exposing itself as a
swappable seam rather than a hardwired branch.

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
| Surfaces | GPUI desktop app, Ratatui TUI, headless one-shot CLI (`README.md` § Quick Start). |
| Providers | OpenAI (device-login OAuth), OpenCode Go (`opencode-go/`), Antigravity (`antigravity/`); unified `ProviderClient` router. |
| Durable core | Harness V2: append-only JSONL + SQLite, intent-first records (`OperationStarted`, `StepAttempt`, `ToolStarted`), replay/recovery, subagents/lanes, per-run cancellation, compaction (`TokenBudget`, `SemanticKeyframes`). |
| Tools | `read_file`, `write_file`, `edit_file_hashline`, `grep_search`, `run_command`, `task`/`subagent`, MCP, `plan`, and memory via `.threadlane/memory.md`. |
| WASI extensions | `lsp_ext`, `debug_ext`, `goal_ext`, `web_ext`, `broker_smoke_ext`; capability broker with named managed processes (`process.spawn`/`recv`/`send`). |
| Skills | Global + project-scoped discovery, per-project enable/disable, slash-command completion. |
| ACP | External agent-as-subprocess client (`acp/<agent_id>`), handshake probe, defensive decode. |
| Shipping | Signed updater, Release Please, macOS/Linux bundling. |

### Known gaps (measured, not assumed)

- **Panic blast radius:** ~1,949 `.unwrap()`/`.expect()` sites and 26 `panic!` across
  crates, concentrated in the newest harness/agent/GPUI layers. A corrupt or mid-write
  session file must degrade, never crash, to honor the "never lose sessions" invariant.
- **`model-visible means logged` is a documented goal but not yet a worked invariant.**
  `harness_v2.md` asserts the log is canonical; we do not yet have a runtime/test that
  *asserts* every model-visible input is reconstructable from the log.
- **Search is a fork/exec tax.** `grep_search` shells out; `omp` links ripgrep/glob
  in-process and reports it as a first-try-edit win.
- **No LSP-on-write.** `lsp_ext` exists but diagnostics do not auto-fire after an edit.
- **No preview-then-accept edits.** Edits apply immediately; `omp`'s staged `ast_edit`
  + `xd://resolve` Accept card is the reference.
- **No virtual `://` schemes** (`pr://`, `agent://`, `skill://`, `ssh://`).
- **No stream rules** (mid-token abort + inject) and **no durable memory bank**
  (`retain`/`recall`/`learn`); memory is a static `.threadlane/memory.md`.
- **No collaboration** (`/collab`), though the ACP client is a close relative.
- **No fallback chains / role-routed model sets** (we have a single selected model).

---

## 3. Gap Analysis vs. the References

Legend: ✅ have · 🟡 partial · ❌ missing · — not applicable to a native Rust surface.

| Reference capability | Threadlane | Learn it | Priority |
| --- | --- | --- | --- |
| Append-only session log as source of truth | 🟡 (V2 JSONL exists) | enforce "model-visible ⇒ logged" invariant | **P0** |
| Capability seams (FS/subprocess/sandbox/approval/LLM) | 🟡 (ad-hoc, not seams) | extract seam boundaries | **P0/P1** |
| Defensive decode / graceful degradation | 🟡 | panic-guard + fuzz the replay boundary | **P0** |
| Boot-time observable composition (`--dump-config`) | ❌ | dump harness composition | **P1** |
| Hash-anchored edits (stale-anchor rejection) | ✅ (`edit_file_hashline`) | keep; add preview-then-accept | 🟡→P1 |
| In-process search/coreutils (no fork/exec) | ❌ (`run_command`, `grep_search` shell out) | in-process ripgrep/glob | **P1** |
| LSP wired into every write (rename, diagnostics) | 🟡 (`lsp_ext` exists) | auto-diagnostics post-edit | **P1** |
| Real DAP debugger drive (breakpoints/stepping) | 🟡 (`debug_ext` exists) | verify multi-step resume UX | **P2** |
| Agent-curated memory (`retain`/`recall`/`learn`) | ❌ (static `memory.md`) | memory bank + compaction survival | **P2** |
| Stream rules (abort + inject mid-token) | ❌ | stream-rule hook in the loop | **P2** |
| Virtual `://` schemes (`pr://`, `agent://`, `skill://`) | ❌ | scheme resolver behind `read` | **P2** |
| Provider fallback chains + role routing | 🟡 (single model; multi-provider) | role sets + fallback chains | **P2** |
| Atomic commit splitting (dependency-ordered) | ❌ (single commit dialog) | split changes into ordered commits | **P3** |
| Session collaboration (`/collab`) | ❌ (ACP client only) | relay-based share/join | **P3** |
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
5. **Measure before changing.** Reuse `threadlane-mcp`/`threadlane-agent` perf baselines
   and add behavioral (not timing) tests for fixes — our existing `AGENTS.md` rule and
   `omp`'s first-exec-cost warning.

---

## 5. Milestones

Each milestone has an explicit **exit gate** that must pass before the next begins.
Milestones are ordered by leverage, not effort: P0 stabilizes the surface we already
have; P1/P2 add flywheel capabilities; P3 is parity/ambition.

### P0 — Hardening the durable core (learn from `dsh`)

**Why first:** everything else builds on the harness; a panic in replay loses sessions.

- [ ] **Replay-boundary panic guard.** Audit `SessionLine`/harness JSONL parsing and ACP
      message decode for panicking paths; convert to `Result`/skip-and-record.
      **Gate:** truncated, interleaved (V1+V2), and mid-write session files all open
      without a panic, via a property/fuzz test feeding random/corrupt inputs to the parser.
- [ ] **"Model-visible ⇒ logged" invariant test.** Add a test that projects model history
      from the log and asserts every message the loop sent is present there.
      **Gate:** the test fails if any model-visible message bypasses the log.
- [ ] **Crash-recovery conformance for subagents.** Ensure child-lane lanes follow the
      canonical `SessionAgent` path with deterministic identity (already documented);
      add a test that a killed child resumes without duplicating durable records.
      **Gate:** kill-mid-write test leaves a replayable log with no orphaned sequences.
- [ ] **Panic/unwrap census.** Add a CI metric (or a script) counting `unwrap`/`expect`/
      `panic!` in `threadlane-agent`, `threadlane-coding-agent`, `threadlane-wasi`.
      **Gate:** count trends down, not up, on each PR touching those crates.

### P1 — The tool flywheel (learn from `omp`)

- [ ] **In-process search.** Replace the `grep_search`/`glob` fork/exec path with
      in-process ripgrep-style search and glob, keeping the same tool schema.
      **Gate:** a behavioral test counts zero child-process spawns for a search, and the
      existing perf baseline shows ≥5× improvement over `rg --` fork/exec on a warm run.
- [ ] **Preview-then-accept edits.** Give `edit_file_hashline` (and a structural edit if
      we add one) a staged "proposed" result and a small accept/apply step, rather than
      applying immediately when in an interactive surface.
      **Gate:** default remains immediate for headless; interactive surfaces show a
      proposed → Accept card; stale-anchor rejection still applies.
- [ ] **LSP-on-write.** Auto-trigger `lsp_ext` diagnostics after a file edit lands and
      surface them in the chat/right panel without blocking the turn.
      **Gate:** an edit that introduces a compile error surfaces a diagnostic within the
      same turn; no blocking regression to the agent loop.
- [ ] **Boot-time harness composition dump.** Add a `threadlane --dump-config` (or debug
      log) that prints the resolved harness composition: active lane, session file,
      skills, extensions, provider, and sandbox policy — `dsh`'s `--dump-config`.
      **Gate:** `--dump-config` output is stable and greppable by CI.

### P2 — Memory, stream rules, and routing (learn from `omp` + `dsh`)

- [ ] **Agent-curated memory bank.** Replace/supersede static `.threadlane/memory.md`
      with a durable bank: `retain` (queue a fact), `recall` (search), `learn` (capture a
      lesson, optionally promote to a skill), `memory_edit`. Survive compaction and
      reload from the session log.
      **Gate:** a fact retained in one turn is `recall`ed in a fresh session over the same
      project; memory is project-scoped.
- [ ] **Stream rules.** Add an abort-and-inject hook: a project rule matches mid-stream,
      aborts the current request, injects the rule as a system reminder, and retries from
      the same point; injections survive compaction (`omp` feature 04).
      **Gate:** a matching rule fires mid-token and the corrected completion lands without
      duplicating prior output.
- [ ] **Role routing + fallback chains.** Add per-role model sets (default / smol / slow /
      plan / commit) and per-provider fallback chains that take over the turn on 429/
      quota, restored on cooldown (`omp` features "four knobs").
      **Gate:** a failing primary transparently fails over to the next provider for the
      same turn; role selection persists per session like the current model id.
- [ ] **Virtual `://` schemes.** Resolve `pr://`, `issue://`, `agent://`, `skill://`
      inside the existing FS-shaped read path so the agent doesn't learn new tools
      (`omp` feature 17). GitHub PRs/issues first; `agent://` field extraction later.
      **Gate:** `read pr://N` returns the same shape as `read <file>`; unknown schemes
      degrade to a clear error.

### P3 — Collaboration, commits, and parity (ambition)

- [ ] **Atomic commit splitting.** Split a working tree's unrelated changes into
      dependency-ordered atomic commits, sourced files first, lock files excluded
      (`omp` feature 16). Complements the existing single "Commit" dialog.
      **Gate:** a mixed diff becomes ≥1 valid commit with no cycle before write.
- [ ] **Session sharing (`/collab`).** A read-only/read-write link + `join` command over
      a sealed-frames relay, the agent never revealing credentials (`omp` feature 07).
      **Gate:** a second client can watch a live session; destructive actions still gate
      on permission.
- [ ] **Surface parity.** Feature parity across GPUI desktop, TUI, and headless: same
      durable records, same tool surface, same plan/activity rendering.
      **Gate:** a parity checklist in CI asserts the three surfaces expose the same tool
      set and session model.

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
   GPUI, TUI, and headless.

---

## 8. Keeping This Current

Treat this file like `AGENTS.md`: living documentation. When a milestone ships, tick it
with the commit, and when a reference introduces a capability we adopt *or reject*, note
that decision here so future readers understand why.
