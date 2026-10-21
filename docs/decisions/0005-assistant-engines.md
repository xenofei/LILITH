---
status: accepted
date: 2026-10-12
governs: AI-003, AI-046, AI-047, AI-096, AI-104, AI-105, AI-199, ARCH-008, ARCH-062
---
# 0005 Claude Code on your own sign-in as the default assistant engine

## Context
The assistant needs a strong tool-using model. You already use Claude Code on a Claude Pro/Max subscription, which covers its use at no extra cost per call. Anthropic's terms allow signing in to the unmodified Claude Code binary with your own subscription, and forbid third-party apps from using or relaying subscription credentials (checked 2026-10-12, AI-047).

## Options considered
- **Use the subscription sign-in directly from the suite.** Rejected: the terms forbid it, and the suite would hold your credentials.
- **Long-lived `claude setup-token` tokens.** Rejected: the suite would have to store them, and they lose plan features (AI-096).
- **The Anthropic API only.** Kept as Engine B, an alternative: every token is paid.
- **Run your installed, unmodified `claude` CLI headless as Engine A**, with Engine B and any `openai_chat` route (Engine C) as alternatives. Chosen.

## Decision
TETHER and MAGI Core default to Engine A; a new ORACLE chat starts on the local model (AI-003, AI-046). The suite never reads, stores or forwards your Claude Code sign-in (ARCH-062, AI-047), and it never chooses a Claude Code profile for you (AI-105).

## Consequences
- The suite depends on Claude Code's headless interface: it accepts a tested version range only, and the M0 spike pins the flags (§4.8).
- Plan limits are Claude Code's; when one is reached, you choose to wait for the reset, switch to Engine B or use a local model (AI-104).
- Engines B and C share one turn loop in the core (AI-199), so a change of engine keeps the tools, confirmations and conversation store.
