# Threadlane (`mypi`) Technical Roadmap

Strategic vision and execution plan to match and exceed the **oh-my-pi (`omp`)** AI agent harness by leveraging existing Threadlane architecture and open-source community building blocks (**TokenSave**, **RTK**, and **WASI Extensions**).

---

## 1. Current System Capabilities (What We Already Have)

Threadlane is a native Rust workspace centered on a GPUI desktop application. Core existing capabilities include:

* **Hash-Anchored File Editing**: Built-in `edit_file_hashline` tool (`crates/threadlane-tools/src/hashline.rs`) ensuring context-safe, zero-drift file modifications.
* **GPUI Desktop UI**: Native desktop workspace, session sidebar, chat markdown renderer, and real-time collapsible `Working`/`Worked` subagent activity rails.
* **WASI Extension Runner**: Sandboxed extension system (`extensions/` targeting `wasm32-wasip1`) for modular tooling and LSP extension plugins.
* **Model Context Protocol (MCP)**: Native MCP client (`crates/threadlane-coding-agent/src/mcp.rs`) with built-in configuration for **TokenSave** Code Graph server.
* **Task Supervisor & Subagent Worktrees**: `HarnessSupervisor` managing subagents with isolated git worktrees (`inherit`, `branch`, `share` modes).
* **Multi-Provider LLM Engine**: Unified integration for Anthropic, OpenAI, Google Gemini, Ollama, OpenRouter, and custom endpoints via `crates/threadlane-provider`.

---

## 2. Community-Driven Integration Strategy

Instead of re-inventing complex tooling engines from scratch, Threadlane leverages established open-source community solutions:

| Feature Area | Conventional Approach | Threadlane Community-First Strategy |
| :--- | :--- | :--- |
| **AST & Code Graph Intelligence** | Custom Tree-sitter AST parsers | **TokenSave (`.tokensave`)**: Deep SQLite code graph indexing and semantic token saving via native MCP integration. |
| **Command Trimming & Log Pruning** | Manual regex/substring filters | **RTK (Response Trimmer Kernel)**: Open-source CLI output trimming engine for `cargo check`, `npm test`, and `grep`. |
| **Language Server Protocol (LSP)** | Monolithic embedded LSP client | **Threadlane WASI Extension Engine**: Modular WASM extensions (`extensions/`) loading LSP language plugins on demand. |

---

## 3. Four-Phase Execution Roadmap

### Phase 1: Open-Source Harness Integration (Target: Q3 2026)
- [ ] **TokenSave Code Graph Deepening (`.tokensave`)**
  - Wire `tokensave` MCP server tools into the default coding agent prompt templates.
  - Enable AST symbol searching, dependency graph traversal, and token-saving context queries via TokenSave index.
- [ ] **RTK Command Trimmer Integration**
  - Integrate **RTK** into `run_command` in `crates/threadlane-tools`.
  - Automatically filter and trim noisy build outputs, compiler logs, and test results before injecting into LLM context.
- [ ] **WASI LSP Extension Ecosystem**
  - Standardize WASI extension contracts for language servers (`rust-analyzer`, `tsserver`, `gopls`).
  - Auto-trigger diagnostic checks post file modification through WASI LSP extensions.
- [ ] **Universal Config Auto-Discovery**
  - Extend `crates/threadlane-coding-agent/src/skills.rs` to automatically discover `.cursorrules`, `.clauderc`, and `Windsurf` rules alongside `AGENTS.md`.

### Phase 2: Context Optimization & Safety Guardrails (Target: Q3/Q4 2026)
- [ ] **Zero-Match & Noise Compaction**
  - Utilize RTK and token counting to compress empty `grep_search` results, redundant file reads, and repeated tool failures.
- [ ] **Subagent Path-Pinning & Least Privilege**
  - Enforce strict working-directory boundaries on subagents to prevent file edits outside designated `git worktree` roots.
  - Implement JSON Schema validation for subagent return payloads.
- [ ] **Persistent Session Memory**
  - Auto-synthesize key architectural decisions and project-specific patterns into persistent session memory (`.threadlane/memory.json`).

### Phase 3: Headless Stdio RPC Engine (Target: Q4 2026)
- [ ] **Decoupled Agent Engine Core**
  - Separate `crates/threadlane-coding-agent` execution loop from desktop UI dependencies.
  - Create `threadlane-cli` binary target for terminal-native usage.
- [ ] **Headless Stdio RPC Mode (`threadlane --mode rpc`)**
  - Implement NDJSON stdio RPC protocol for external embedding (Neovim, VSCode, CI/CD runners).
  - Stream `AgentEvent` updates, tool calls, and approval dialogs over stdio RPC.

### Phase 4: Advanced Tooling & UI Parity (Target: Q1 2027)
- [ ] **Debug Adapter Protocol (DAP) via WASI**
  - Add DAP WASI extension plugin for step-debugging, breakpoint management, and call-stack inspection.
- [ ] **Headless Browser Capability**
  - Integrate Playwright / Chromium CDP via MCP/WASI extension for Web UI testing and rendering verification.
- [ ] **Dual-Mode UI Synchronization**
  - Ensure feature parity between the GPUI Desktop application and the headless CLI/TUI runtime.

---

## 4. Verification & Success Metrics

1. **Compilation & Code Hygiene**:
   - `cargo check --workspace`
   - `git diff --check`
2. **Token Efficiency & Context Reduction**:
   - Measure >40% context token savings on large builds using **RTK** and **TokenSave**.
3. **Edit Safety**:
   - 100% edit accuracy with zero line-drift using `edit_file_hashline`.
