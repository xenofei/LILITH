---
status: accepted
date: 2026-10-20
governs: roadmap "Backlog (frozen until its plan is next)"; QA-028, QA-029 as context
---
# 0006 The spec is written only for the next two plans

## Context
The spec was written in full before the first plan and has grown past a thousand rules. Research and owner direction keep changing what the later apps should be, while tests and plans cite rule ids that must stay stable (QA-028).

## Alternatives considered (agent summary)
- **Keep specifying every later app now.** Those rules would go stale before any code needs them.
- **No spec beyond the plans.** Protected tests, reviews and the traceability report need one stable source of rule ids (QA-028).
- **Freeze the spec beyond the next two plans**, and keep later work as notes. Chosen.

## Decision
The roadmap's "Backlog" section (owner decision 2026-10-20) is the binding text.

## Consequences
- Later ideas live in the roadmap backlog, each with a verdict and a target plan.
- Writing a plan starts with a spec change for its rules, which lands first (QA-029).
- Rules for frozen areas are revised when their plan is next, not before.
