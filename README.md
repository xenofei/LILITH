# LILITH

LILITH (Latent Image & Likeness Instrumentality Toolchain Hub) is a desktop creative suite at personal-hobby scale: a family of AI-enabled apps, starting with an Image studio, that share one workspace of projects and assets. It is Windows-first.

- **Rust core.** A headless core service (`lil-core`, one per user) owns all state and drives the engines (ComfyUI first). Apps, the `lil` command line and AI agents are all clients of the same tool surface.
- **Qt apps.** The apps are Qt 6 / QML, bound to Rust with cxx-qt.
- **AI where it helps.** Hosted and local models go through one tool surface, with usage and cost tracking and optional spending caps. Claude Code is the default assistant engine, run as the user's own unmodified CLI with their own sign-in. The suite adds no content filter of its own; each provider's and model's own policies and licences apply, and you are responsible for what you make with them.

## Status

Early. The M0 spikes (toolchain, Qt stack, GPU canvas, ComfyUI measurements) are done, and plan 1 is building the repository foundation: the Cargo workspace with the first Qt-free crates, spec tests, the protected-test merge guard, git hooks and CI. There is no usable app yet.

## Build and test

Requirements: Windows 11 and the Rust toolchain pinned in `rust-toolchain.toml` (rustup installs it). The crates in this workspace don't need Qt; the throwaway programs under `spikes/` do (see `spikes/README.md`).

```powershell
cargo build --workspace
cargo nextest run --workspace        # needs cargo-nextest; or: cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo deny check                     # needs cargo-deny
cargo run -q -p lil-cli -- dev spec-report
```

## Merging into main

Changes reach `main` only through pull requests merged with `lil dev merge <PR>`, which checks CI, the protected-file approvals and the pinned head commit first. The merge guard is described in the spec (§6.2); the maintainer's guide, with the one-time setup and everyday use, is [docs/dev/merge-guard.md](docs/dev/merge-guard.md).

Before working in a clone that pushes:

1. Store the agent token with `cargo run -q -p lil-cli -- dev agent-token set` (at a terminal; it is kept in Windows Credential Manager).
2. Run `cargo run -q -p lil-cli -- dev install-hooks`. It points git at the repository's hooks and sets the repository's git credential helper to the agent-token helper (`lil dev git-credential`), so git in this clone authenticates to GitHub only with that token.
3. Create the private denylist `%USERPROFILE%\.lilith\privacy-denylist.txt`, one entry per line. The `pre-push` hook runs `lil dev privacy-scan` on every pushed ref and refuses every push until this file exists.

Run `lil` from the checkout it was built in (`cargo run` there): a `lil` built in one checkout refuses to work on another.

## Documentation

- **Design spec:** [docs/superpowers/specs/2026-10-12-lilith-design.md](docs/superpowers/specs/2026-10-12-lilith-design.md). Every requirement has a stable rule ID.
- **Plans:** [docs/superpowers/plans/](docs/superpowers/plans/), starting with the [roadmap](docs/superpowers/plans/2026-10-12-roadmap.md).
- **M0 spike results:** [docs/spikes/README.md](docs/spikes/README.md).
- **Research notes** behind the spec, dated with their sources: [docs/research/](docs/research/).

## Licence

Copyright (C) 2026 the LILITH contributors.

This program is free software: you can redistribute it and/or modify it under the terms of the GNU Affero General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version.

This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more details.

Releases published before the relicensing commit were published under GPL-3.0-or-later, and those copies stay under that licence; from that commit onward, the code is AGPL-3.0-or-later.

The full licence text is in [LICENSE](LICENSE). Third-party material in this repository is listed in [NOTICE](NOTICE).
