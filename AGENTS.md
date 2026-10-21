# Agent instructions for LILITH

LILITH is a personal creative suite: a Rust core service (`lil-core`) and Qt apps. This repository is public, and most of its code is written by agents. These rules apply to every change.

## Commands

Run `lil` through cargo from this checkout: a `lil` built in another checkout refuses this one.

- **Tests: targeted only.** Run the tests that cover the files you touched, by crate and filter, for example `cargo nextest run -p lil-cli --retries 0 -E 'test(/spec_report/)'`. Never run the full workspace suite; CI runs it on every PR.
- `cargo run -q -p lil-cli -- dev spec-tests`: the protected spec tests and the expected-fail list.
- `cargo run -q -p lil-cli -- dev spec-report`: rule counts, dangling rule citations and private ruling citations. It must report 0 of each.
- `cargo run -q -p lil-cli -- dev privacy-scan --tree HEAD`: nothing personal in tracked files. Run it before every commit you push.
- `cargo clippy -p <crate> --all-targets -- -D warnings` and `cargo fmt -p <crate>` for each crate you changed.
- **Never run `lil dev approve` or `lil dev merge`.** You push and open PRs; the owner approves and merges.
- Files listed in `dev/protected-paths.txt` need the owner's approval at the PR. Say in the PR which ones you changed.

## Rule ids

- The spec is `docs/superpowers/specs/2026-10-12-lilith-design.md`. Every requirement is a bullet with a stable id such as `CORE-042`, tagged with a milestone and a kind (§0.3).
- Cite ids, don't paste rule text: `(CORE-026)` in prose and briefs, `// spec: CORE-026, ARCH-042` on the line of a test that checks them.
- A new rule takes the next free number of its prefix. Ids are never reused or renumbered; a removed rule is marked withdrawn.
- When the rule count changes, update `EXPECTED_RULE_COUNT` in `crates/lil-cli/src/dev/spec_report.rs`.
- Spec changes land in their own commit, before the code that depends on them (QA-029).

## Terms and decisions

- **Terms** live only in `docs/GLOSSARY.md`. Use the term its row names, never a name in its Avoid column. A new term enters the glossary in the same change that introduces it (QA-089).
- **Decisions** are spec rules tagged `(decision)`; they are binding. `docs/decisions/` holds short records for the few cross-cutting ones, with the options that were rejected.
- **Rulings** made while a plan runs are recorded with their reasons in that plan's "Decisions recorded" section. Text may cite a ruling id such as `P3-R9` only when that section records it (QA-090; `dev spec-report` checks).
- **Stop and ask the owner** when a task conflicts with a `(decision)` rule, a decision record or a recorded ruling. Propose the change; never build around the decision.

## Nothing personal in the repo

Every tracked file, path and commit is public (QA-086). Never write a real name, e-mail address (other than a GitHub no-reply address), Windows user-profile path, account detail or anything about the owner's machine or habits. Write `<owner>`, `%USERPROFILE%`, `%APPDATA%` and `%LOCALAPPDATA%` instead. Briefs, reports, ledgers and research stay outside the repository.
