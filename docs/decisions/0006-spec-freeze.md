---
status: accepted
date: 2026-10-20
governs: QA-028, QA-029
---
# 0006 The spec is written only for the next two plans

## Context
The spec was written in full before the first plan and has grown past a thousand rules. Research and owner direction keep changing what the later apps should be, and a rule written long before its plan has to be rewritten when the plan comes, while tests and plans cite rule ids that must stay stable.

## Options considered
- **Keep specifying every later app now.** Rejected: those rules go stale before any code needs them, and each rewrite churns the spec.
- **No spec beyond the plans.** Rejected: protected tests, reviews and the traceability report need one stable source of rule ids (QA-028).
- **Freeze the spec beyond the next two plans**, and keep later work as notes. Chosen.

## Decision
The spec is frozen beyond the next two plans, 4a and 4b (owner decision 2026-10-20). Work for later plans is kept in the roadmap's backlog, and a note becomes spec rules only when its plan is next. The 2D module, the next studio, gets its rules then.

## Consequences
- The roadmap backlog is where later ideas live, each with a verdict and a target plan.
- Writing a plan starts with a spec change for its rules, which lands first (QA-029).
- Rules for frozen areas stay as they are; they are revised when their plan is next, not before.
