---
name: goal-writer
description: Drafts and reviews strong /goal objectives for Threadlane goal_ext. Use when the user asks to write, improve, audit, or meta-prompt a long-running agent goal with clear success criteria, verification surface, constraints, and completion audit requirements.
---

# Goal Writer

## Purpose

Write `/goal` prompts that are fit for persistent autonomous execution in Threadlane.
A goal is a completion contract: the agent autonomously loops turns until every requirement is verified against real evidence or stopped by constraints.

## Core Rules

Never produce a vague goal like "improve the codebase" or "make it better".
A strong goal consists of six components:
1. **Outcome**: What must be true when the work is done.
2. **Verification surface**: Commands (`cargo test`, `cargo check`), diff audits, screenshots, logs, or explicit output.
3. **Constraints**: What must not regress or change.
4. **Boundaries**: Allowed directories, files, tools, and systems.
5. **Iteration policy**: How to prioritize the next action across turns.
6. **Blocked stop condition**: When to stop with evidence rather than drifting.

## Usage Shape

```text
/goal [--tokens 50k] <desired end state>, verified by <specific evidence>, while preserving <constraints>.
```

## Completion Contract

The agent will audit against real evidence and will not stop until `update_goal(status="complete", evidence="...")` is called.
