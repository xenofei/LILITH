# Spec tests

Spec tests describe behaviour from the outside. They may use only:

- the `lil` CLI (run as a child process),
- file formats (reading and writing the documented on-disk files),
- events,
- `lil-protocol` descriptors,
- the `lil-testkit` API.

They must not depend on `lil-core` or any other implementation crate (QA-018).
Each test cites the rule it covers with a `// spec: QA-xxx` comment.
Known failures are listed in `../expected-fail.toml`; run them with `lil dev spec-tests`.

Entries in `expected-fail.toml` use the fully qualified name, `<binary>::<test file>::<test fn>` (for example `lil-spec-tests::smoke::spec_harness_runs`). An entry that matches no test is reported as stale and fails the run.
