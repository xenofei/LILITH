# Decision records

Short records of the few cross-cutting decisions that are hard to reverse and had real alternatives. Each gives the context, the options considered, the decision, its consequences and the spec rules it governs.

- **The spec stays binding.** A decision is stated by its `(decision)` rule in the spec (`docs/superpowers/specs/2026-10-12-lilith-design.md`). A record here explains it and never restates the rule; where they seem to differ, the rule wins and the record is fixed.
- **When to write one:** only when a decision is hard to reverse, surprising without context and a real trade-off, and its reason won't fit in a line or two of the rule. Most decisions don't need one: a one-sentence "Why:" under the rule is enough.
- **Rulings** made while a plan runs are recorded in that plan's decisions section, not here (QA-090).
- **Numbering:** four digits, in order, never reused. A record is superseded by a new one rather than rewritten; a dated addition at its end is fine.
- **Agents:** a task that conflicts with an accepted record or its rules stops and asks the owner (`AGENTS.md`).

## Records

| No. | Decision | Status |
|---|---|---|
| [0001](0001-licence-agpl.md) | The suite's code is AGPL-3.0-or-later | accepted 2026-10-20 |
| [0002](0002-public-repo-private-records.md) | A public repository, with the working records kept private | accepted 2026-10-13 |
| [0003](0003-hosted-first-routing.md) | Hosted models first for non-adult work | accepted 2026-10-20 |
| [0004](0004-wrapped-engines.md) | Wrap engines such as ComfyUI, owned or attached | accepted 2026-10-12 |
| [0005](0005-assistant-engines.md) | Claude Code on your own sign-in as the default assistant engine | accepted 2026-10-12 |
| [0006](0006-spec-freeze.md) | The spec is written only for the next two plans | accepted 2026-10-20 |
| [0007](0007-agent-built-workflow.md) | Plans are built by agents, task by task, under protected tests | accepted 2026-10-12 |

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
- Option, with why it was rejected (or chosen).

## Decision
One or two sentences. The rules above are the binding text.

## Consequences
What follows, including what it costs if it turns out wrong.
```
