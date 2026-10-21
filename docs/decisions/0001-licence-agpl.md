---
status: accepted, applied by the relicence PR
date: 2026-10-20
governs: OPS-108
---
# 0001 The suite's code is AGPL-3.0-or-later

## Context
The suite has been free software since the repository went public (Amendment A, 2026-10-13), under GPL-3.0-or-later. The GPL's copyleft applies when a binary is distributed; it doesn't reach a modified version that someone only runs for others over a network.

## Alternatives considered (agent summary)
- **Keep GPL-3.0-or-later** (the earlier choice). Rejected: a modified LILITH offered over a network would owe its users no source.
- **AGPL-3.0-or-later.** Chosen: the same terms, plus section 13, which makes whoever runs a modified version for users over a network offer them its source.

## Decision
The suite's own code is licensed AGPL-3.0-or-later (owner decision 2026-10-20). Releases published before the relicensing change stay under GPL-3.0-or-later. The relicence PR applies it: it updates `LICENSE`, the workspace `license` field, the plugin manifest, the README, `NOTICE` and OPS-108 together. Until that PR merges, OPS-108 states GPL-3.0-or-later and is the binding text.

## Consequences
- Anyone who runs a modified LILITH for others over a network must offer them its source.
- Dependencies stay compatible: the Rust dependencies are permissive or Apache-2.0; Qt's GPL parts combine with the AGPL through section 13 of both licences; LGPL Qt is linked dynamically. `cargo-deny` doesn't see Qt, so a release checks it by hand.
- Closed runtimes are still never linked into the suite's binaries (OPS-108).
