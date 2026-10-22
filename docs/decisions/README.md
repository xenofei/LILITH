# Decision records

Short records of the few cross-cutting decisions that are hard to reverse and had real alternatives. Each gives the context, the options considered, the decision, its consequences and the spec rules it governs.

- **The spec stays binding.** A decision is stated by its `(decision)` rule in the spec (`docs/superpowers/specs/2026-10-12-lilith-design.md`). A record here explains it and never restates the rule; where they seem to differ, the rule wins and the record is fixed.
- **When to write one:** only when a decision is hard to reverse, surprising without context and a real trade-off, and its reason won't fit in a line or two of the rule. Most decisions don't need one: a one-sentence "Why:" under the rule is enough.
- **Rulings** made while a plan runs are recorded in that plan's decisions section, not here (QA-090).
- **Numbering:** four digits, in order, never reused. A record is superseded by a new one rather than rewritten; a dated addition at its end is fine.
- **Agents:** a task that conflicts with an accepted record or its rules stops and asks the owner (`AGENTS.md`).
- **Sources:** a section headed "(agent summary)" is an agent's reading of the sources, not the owner's words; the owner confirms or corrects it.
- **Index:** list this folder; each file's number and title are the index. There is no hand-kept table.

## Template

```markdown
---
status: accepted | superseded by NNNN
date: YYYY-MM-DD
governs: RULE-ID, RULE-ID
---
# NNNN Title

## Context
What forced a choice, in a few sentences.

## Options considered
- Option, with why it was rejected (or chosen). Head the section "Alternatives considered (agent summary)" when the sources don't state the reasons.

## Decision
One or two sentences. The rules above are the binding text.

## Consequences
What follows, including what it costs if it turns out wrong.
```
