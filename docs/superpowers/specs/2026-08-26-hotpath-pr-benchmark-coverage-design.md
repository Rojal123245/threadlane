# Hotpath PR Benchmark Coverage

## Goal

Every pull request reports comparable performance changes for the repository's deterministic, headless hot paths. The suite should expose regressions without requiring UI automation, network access, or a new benchmark framework.

## Scope

Add Hotpath examples for four suites:

- `runtime`: JSONL append/open, reducer replay, and session reload.
- `tools`: warm in-process repository search.
- `mcp`: warmed discovery/reconnect and steady-state tool calls against a local stub server.
- `terminal`: VT100 parsing and resize/scrollback work without launching GPUI.

Benchmarks use fixed realistic workloads and `std::hint::black_box` where needed. Filesystem and subprocess setup occurs outside measured functions whenever Hotpath permits it. Existing production helpers are reused; benchmark-only public APIs are not added unless no narrower option exists.

## CI and Reporting

Convert `hotpath-profile.yml` to a matrix with one entry per suite. Each entry runs the same example on the PR head and base SHA and uploads uniquely named JSON files plus the PR number.

`hotpath-comment.yml` downloads all metrics and calls the existing `hotpath-utils profile-pr` once per complete base/head pair. Stable benchmark IDs (`runtime`, `tools`, `mcp`, and `terminal`) make each suite update its own PR comment instead of colliding with the others.

A base branch that lacks a newly added suite is skipped for that suite. Other suites still report. Benchmark command failures fail profiling rather than silently publishing incomplete measurements.

## Validation

- Run every benchmark example locally in release mode with Hotpath enabled.
- Parse the emitted JSON through `hotpath-utils profile-pr --dry-run` using a file as both base and head.
- Run focused checks for every touched crate.
- Run `git diff --check`.

## Deliberate Limits

The first version excludes full GPUI rendering, real provider/network calls, and cold machine startup. Those measurements are environment-sensitive and would make per-PR comparisons noisy. Add them only when a stable headless harness and controlled runner exist.
