---
status: accepted
date: 2026-10-12
governs: QA-017, QA-018, QA-019, QA-020, QA-021, QA-023, QA-028, QA-033, QA-034, QA-035, QA-037, QA-079
---
# 0007 Plans are built by agents, task by task, under protected tests

## Context
AI agents write most of the suite's code. The owner steers and tests milestone builds, and quality has to come from the process rather than from reading every line (§0.1). An agent's context runs out on a long plan, an agent reviewing its own work misses its own mistakes, and an agent under pressure can weaken the tests that judge it.

## Options considered
- **One long agent session per plan.** Rejected: context is lost on the way, and nobody independent reviews the work.
- **The owner reviews every line.** Rejected: it doesn't scale, and it isn't the owner's role (§0.1).
- **Task-by-task agents under protected tests, independent review and a guarded merge.** Chosen.

## Decision
A controller agent runs each plan task by task: a fresh implementer per task, then an independent reviewer with a different model or fresh context who tries to break it with a failing test (QA-023 makes this mandatory for the core's riskiest areas). Protected spec tests are written from the spec by an author who doesn't read the implementation (QA-017, QA-018), and a change that modifies or deletes a protected file needs the separate reviewer identity's approval (QA-019, QA-020, QA-021). Agents push and open PRs; they never run `lil dev approve` or `lil dev merge`, which the owner runs from a clean checkout of `main` (QA-033). Implementers run only the tests that cover what they touch, and CI runs the full suite on every PR (plan 3 ruling R-T).

## Consequences
- Each plan has a private ledger of tasks, reviews and rulings; the rulings that public text cites are recorded in the plan (QA-090).
- The git hooks, CI after every merge and server-side rulesets back the merge guard (QA-034, QA-035, QA-037).
- Agents' GitHub tokens can't change workflow files or repository settings (QA-079), so those changes go through the owner.
- A cross-crate break can surface in CI rather than locally.
