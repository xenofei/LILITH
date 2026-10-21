---
status: accepted
date: 2026-10-20
governs: AI-232, AI-002, AI-233, AI-036, CORE-057
---
# 0003 Hosted models first for non-adult work

## Context
Every job runs on a route: a hosted provider's model or a local one. The earlier default was local first. Hosted flagship models usually give the strongest result for a job but cost money per call, and each hosted provider applies its own content policy. Local models cost only energy and apply no policy but the model's own.

## Options considered
- **Local first** (the earlier default). Rejected: most work would start on the weaker option.
- **Route automatically by content**, sending adult work to local models. Rejected: the suite would have to inspect content, and it never blocks or reroutes a job on content grounds.
- **Hosted first for non-adult work, local as an option, and you pick the model for every job.** Chosen.

## Decision
Non-adult assets default to the best hosted flagship model for the job; adult work runs mostly on local models (AI-232). The defaults are ordinary settings-cascade values (CORE-057, AI-036), so a project for adult work sets local defaults. The suite never inspects content to choose a route.

## Consequences
- More work is metered and paid; the meter, optional caps and the dashboard track it (§2.6).
- Every route shows its provider's content-policy label, as information only (AI-233).
- Until plan 6b brings direct hosted image APIs, hosted image generation runs through ComfyUI's partner nodes where one covers the job, otherwise the local profile (AI-232).
- If the choice is wrong, it is a change of default values, not of code paths.
