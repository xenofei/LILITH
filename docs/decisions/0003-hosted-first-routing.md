---
status: accepted
date: 2026-10-20
governs: AI-232, AI-002, AI-233, AI-036, CORE-057
---
# 0003 Hosted models first for non-adult work

## Context
Every job runs on a route: a hosted provider's model or a local one. The earlier default was local first. Hosted models cost money per call, and each hosted provider applies its own content policy. The owner's reason for keeping adult work local (2026-10-20): adult work won't go through flagship APIs, mostly because hosted providers restrict it.

## Alternatives considered (agent summary)
- **Local first** (the earlier default), replaced by this decision.
- **Route automatically by content.** Not possible under AI-232 and AI-002: the suite never inspects content to choose a route, and never blocks or reroutes a job on content grounds.
- **Hosted first for non-adult work, local as an option, and you pick the model for every job.** Chosen.

## Decision
AI-232, with the defaults as settings-cascade values (CORE-057, AI-036).

## Consequences
- More work is metered and paid; the meter, optional caps and the dashboard track it (§2.6).
- Every route shows its provider's content-policy label, as information only (AI-233).
- If the choice is wrong, it is a change of default values, not of code paths.
