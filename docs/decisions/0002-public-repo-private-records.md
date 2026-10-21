---
status: accepted
date: 2026-10-13
governs: OPS-003, ARCH-109, QA-086, QA-087, QA-037, QA-069, QA-090
---
# 0002 A public repository, with the working records kept private

## Context
The suite began in a private repository. Agents write most of its code, and their working records (plan ledgers, task briefs and reports, review diffs, research notes, owner Q&A) are full of machine paths, account details and personal context. A public repository gets GitHub-hosted CI minutes for free and server-side rulesets that hold the merge guard's checks, but every tracked file, path and commit in it is public for good.

## Options considered
- **Keep the repository private.** Rejected: CI would need a minute budget, and the guard would rest on local hooks without server-side rulesets.
- **Publish everything, records included.** Rejected: the records hold personal context and are large, and they can't be cleaned after the fact.
- **Publish the product, keep the records private.** Chosen.

## Decision
The suite repository is public (Amendment A, 2026-10-13): code, spec, plans, the glossary, these records and the developer docs. The working records stay outside the repository's tracked files. Nothing personal enters the repository (QA-086), agents reach GitHub only with a narrow token (QA-087), and a reason the public text depends on is never left only in a private record (QA-090).

## Consequences
- The privacy scan runs in the `pre-push` hook and in CI, because a pushed commit can't be taken back.
- Plans state their decisions and rulings publicly; the private ledger keeps only working detail.
- Public text writes `<owner>`, `%USERPROFILE%` and similar placeholders instead of real names and paths.
