//! Test support shared by the workspace's test suites (QA-031, QA-038, QA-040, QA-056, QA-066):
//! [`TestEnv`] and the harness guard, the test builds of `lil` and `lil-core` ([`bins`]), core
//! fixtures that never outlive their test ([`CoreFixture`], [`KillOnDrop`]), a raw pipe client
//! with the watcher fence ([`RawPipe`]), the fake clock and ids for in-process tests
//! ([`FakeClock`], [`SeqIds`]), fixture workspaces and isolated git ([`workspace`],
//! [`gitops`]), open-handle holders ([`HandleHolder`]), fail points ([`failpoints`]), the
//! owner's test parameters ([`params`]) and UTC date helpers ([`utc`]); and the plan-3 fakes
//! (QA-066): an in-process ComfyUI ([`FakeComfy`], also served by `lil-fake-comfyui`) and the
//! ComfyUI shape fixtures it is checked against ([`comfy_shapes`]), fake
//! hardware ([`FakeHwScript`]), the `ui:test` app ([`TestApp`]), stdio servers under the fake
//! `claude` ([`McpStdio`]), `lil-fake-comfy-cli` and free loopback ports ([`ports`]).

pub mod bins;
pub mod cleanup;
pub mod clock;
pub mod comfy_shapes;
pub mod core;
pub mod env;
pub mod failpoints;
pub mod fake_comfy;
pub mod fake_hw;
pub mod files;
pub mod gitops;
pub mod handles;
pub mod ids;
pub mod params;
pub mod ports;
pub mod raw_pipe;
pub mod test_app;
pub mod utc;
pub mod workspace;

pub use bins::{Bins, ensure_built};
pub use clock::FakeClock;
pub use core::{CoreFixture, KillOnDrop};
pub use env::{HOT_JOURNAL_ROWS, LilChild, LilOutput, TestEnv};
pub use fake_comfy::{FakeComfy, FakeFailure, FakeRequest, Stall};
pub use fake_hw::FakeHwScript;
pub use files::{sha256_bytes, sha256_file};
pub use gitops::GitOps;
pub use handles::HandleHolder;
pub use ids::SeqIds;
pub use ports::free_loopback_port;
pub use raw_pipe::{RawPipe, merge_hello};
pub use test_app::{McpStdio, TEST_APP_CAPABILITIES, TestApp};
pub use workspace::version_content_hash_of;
