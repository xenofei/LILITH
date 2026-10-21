---
status: accepted
date: 2026-10-12
governs: ARCH-064, ARCH-065, ARCH-066, ARCH-068, ARCH-069, ARCH-071, ARCH-126
---
# 0004 Wrap engines such as ComfyUI, owned or attached

## Context
Image generation needs a model runtime with a large, fast-moving ecosystem of nodes and models. ComfyUI is strong and maintained, and you may already run your own copy, started by a script or by an MCP server, with jobs of your own in its queue. Later engines (local LLM servers, Blender) raise the same question.

## Options considered
- **Build the inference stack in-house.** Rejected: it would chase an ecosystem that ComfyUI already tracks.
- **Wrap ComfyUI, and run only the copy the suite starts.** Rejected: your own ComfyUI and its outside jobs would conflict with the suite over the GPU.
- **Wrap it in two classes: owned (the core started it) and attached (someone else did).** Chosen.

## Decision
The suite wraps strong, maintained engines and builds in-house otherwise (§0.2). An owned engine the core may start, health-check, restart and idle down; an attached engine it uses and observes but never stops (§1.4, ARCH-064, ARCH-065). Outside jobs in ComfyUI's queue are observed, scheduled around and never cancelled (ARCH-069).

## Consequences
- The suite pins and tests one ComfyUI version, and moving it is a deliberate change (ARCH-065).
- One heavy GPU job runs at a time under the GPU lease, shared with outside work the suite can only observe (ARCH-071).
- Engines run on loopback only with scrubbed environments (ARCH-063, ARCH-068), so a wrapped engine can't spend a paid key outside the caps.
