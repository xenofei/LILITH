---
status: accepted
date: 2026-10-12
governs: QA-017, QA-018, QA-019, QA-020, QA-021, QA-023, QA-028, QA-033, QA-034, QA-035, QA-036, QA-037, QA-079
---
# 0007 Plans are built by agents, task by task, under protected tests

## Context
AI agents write most of the suite's code. The owner steers and tests milestone builds, and quality has to come from the process rather than from reading every line (§0.1). An agent's context runs out on a long plan, an agent reviewing its own work misses its own mistakes, and an agent under pressure can weaken the tests that judge it.

## Alternatives considered (agent summary)
- **One long agent session per plan**, with no independent review.
- **The owner reviews every line.** Rejected by §0.1: quality comes from the process, not from reading every line.
- **Task-by-task agents under protected tests, independent review and a guarded merge.** Chosen.

## Decision
A controller agent runs each plan task by task, with a fresh implementer per task and an independent reviewer (QA-023). The binding rules are the protected spec tests (QA-017, QA-018), the protected files and their approval (QA-019 to QA-021), the merge command (QA-033), the owner-only approve and merge (QA-036), and targeted tests for implementers (plan 3 ruling R-T).

## Consequences
- Each plan has a private ledger; the rulings that public text cites are recorded in the plan (QA-090).
- The git hooks, CI after every merge and server-side rulesets back the merge guard (QA-034, QA-035, QA-037).
- Agents' GitHub tokens can't change workflow files or repository settings (QA-079), so those changes go through the owner.
- A cross-crate break can surface in CI rather than locally.
