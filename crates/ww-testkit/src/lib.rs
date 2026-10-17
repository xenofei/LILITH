//! Test support shared by the workspace's test suites (QA-031, QA-038, QA-040, QA-056):
//! [`TestEnv`] and the harness guard, the test builds of `ww` and `ww-core` ([`bins`]), core
//! fixtures that never outlive their test ([`CoreFixture`], [`KillOnDrop`]), a raw pipe client
//! ([`RawPipe`]), and the fake clock and ids for in-process tests ([`FakeClock`], [`SeqIds`]).

pub mod bins;
pub mod cleanup;
pub mod clock;
pub mod core;
pub mod env;
pub mod files;
pub mod ids;
pub mod raw_pipe;

pub use bins::{Bins, ensure_built};
pub use clock::FakeClock;
pub use core::{CoreFixture, KillOnDrop};
pub use env::{TestEnv, WwOutput};
pub use files::sha256_file;
pub use ids::SeqIds;
pub use raw_pipe::{RawPipe, merge_hello};
