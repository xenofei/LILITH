//! The `meter` module (CORE-014, CORE-069): usage records for every external service call.
//!
//! Plan 3 Task 11 starts it with the shapes a caller hands over ([`sink`]): a [`UsageDraft`]
//! (what was used) and its [`UsageCtx`] (for which job or call), given to a [`UsageSink`].
//! [`LogSink`] is the stand-in until Task 15's `Meter`, which writes the usage lines and the
//! pending rows, implements [`UsageSink`].

pub mod sink;

pub use sink::{Attribution, LogSink, UsageCtx, UsageDraft, UsageSink, UsageStatus};
