//! The client for synchronous callers such as `lil` (ruling P11): no tokio in the caller.
//!
//! [`BlockingClient`] owns a current-thread runtime and runs each call on it, so its
//! connection's reader only runs while a call (or [`Events::next`]) is waiting; the pipe
//! buffers what arrives meanwhile. [`drain`] is `lil core stop`. None of it may be used from
//! inside a tokio runtime (`block_on` panics there).
//!
//! QA-026: an app marks its GUI thread with [`mark_gui_thread`]; in a debug build, every
//! blocking call of `lil-client` panics on that thread before it waits: each call here that
//! waits on the core, and `doctor::core_running` and `doctor::set_repo_autocrlf_false`
//! (APP-024: the GUI thread never waits on the core; an app uses
//! [`crate::session::Session`] on a background runtime).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use lil_protocol::{EventParams, HelloResult, Outcome, ToolDescriptor, ToolOutput, TopicState};
use tokio::runtime::Runtime;

use crate::{Client, ClientConfig, ClientError, DrainOutcome, EventStream, drain_and_wait};

thread_local! {
    /// Whether [`mark_gui_thread`] marked this thread.
    static GUI_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Marks the calling thread as the app's GUI thread (QA-026): from now on, in a debug build,
/// the blocking calls of this module panic when made on it. The mark lasts as long as the
/// thread.
pub fn mark_gui_thread() {
    GUI_THREAD.with(|gui| gui.set(true));
}

/// QA-026: the blocking call `call` must not run on the GUI thread (debug builds only).
pub(crate) fn not_on_gui_thread(call: &str) {
    debug_assert!(
        !GUI_THREAD.with(Cell::get),
        "lil_client::{call} was called on the GUI thread, which never blocks (QA-026, APP-024)"
    );
}

/// A [`Client`] with its own runtime: the same methods, synchronous.
#[derive(Debug)]
pub struct BlockingClient {
    // Dropped before the runtime, so the connection closes while its tasks still exist.
    client: Client,
    rt: Runtime,
}

impl BlockingClient {
    /// [`Client::connect`].
    pub fn connect(cfg: &ClientConfig) -> Result<BlockingClient, ClientError> {
        not_on_gui_thread("blocking::BlockingClient::connect");
        let rt = runtime()?;
        let client = rt.block_on(Client::connect(cfg))?;
        Ok(BlockingClient { client, rt })
    }

    /// [`Client::hello`].
    pub fn hello(&self) -> &HelloResult {
        self.client.hello()
    }

    /// [`Client::call_tool`]: mints an idempotency key when `key` is `None`.
    pub fn call_tool(
        &self,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
        key: Option<String>,
    ) -> Result<Outcome<ToolOutput>, ClientError> {
        not_on_gui_thread("blocking::BlockingClient::call_tool");
        self.rt.block_on(self.client.call_tool(name, args, key))
    }

    /// [`Client::confirmations`].
    pub fn confirmations(
        &self,
        state: lil_protocol::app::ListWhich,
    ) -> Result<Outcome<lil_protocol::app::ConfirmationsListResult>, ClientError> {
        not_on_gui_thread("blocking::BlockingClient::confirmations");
        self.rt.block_on(self.client.confirmations(state))
    }

    /// [`Client::app_call`].
    pub fn app_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<Outcome<serde_json::Value>, ClientError> {
        not_on_gui_thread("blocking::BlockingClient::app_call");
        self.rt.block_on(self.client.app_call(method, params))
    }

    /// [`Client::list_tools`].
    pub fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ClientError> {
        not_on_gui_thread("blocking::BlockingClient::list_tools");
        self.rt.block_on(self.client.list_tools())
    }

    /// [`Client::subscribe`], as a blocking iterator of events that ends when the core goes
    /// away (`lil events --follow`).
    pub fn subscribe(
        &self,
        topics: Vec<String>,
        since: BTreeMap<String, u64>,
    ) -> Result<Events<'_>, ClientError> {
        not_on_gui_thread("blocking::BlockingClient::subscribe");
        let stream = self.rt.block_on(self.client.subscribe(topics, since))?;
        Ok(Events {
            rt: &self.rt,
            stream,
        })
    }
}

/// The events of one subscription, blocking. The iterator ends when the core goes away.
#[derive(Debug)]
pub struct Events<'a> {
    rt: &'a Runtime,
    stream: EventStream,
}

impl Events<'_> {
    /// [`EventStream::topics`].
    pub fn topics(&self) -> &[TopicState] {
        self.stream.topics()
    }

    /// The next event, waiting at most `within`: `Ok(None)` once the core went away,
    /// [`ClientError::Timeout`] when nothing came in time.
    pub fn next_within(&mut self, within: Duration) -> Result<Option<EventParams>, ClientError> {
        not_on_gui_thread("blocking::Events::next_within");
        self.rt
            .block_on(async { tokio::time::timeout(within, self.stream.next()).await })
            .map_err(|_| ClientError::Timeout)
    }
}

impl Iterator for Events<'_> {
    type Item = EventParams;

    fn next(&mut self) -> Option<EventParams> {
        not_on_gui_thread("blocking::Events::next");
        self.rt.block_on(self.stream.next())
    }
}

/// [`drain_and_wait`] on a runtime of its own: `lil core stop`.
pub fn drain(cfg: &ClientConfig, within: Duration) -> Result<DrainOutcome, ClientError> {
    not_on_gui_thread("blocking::drain");
    runtime()?.block_on(drain_and_wait(cfg, within))
}

fn runtime() -> Result<Runtime, ClientError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            ClientError::Io(io::Error::new(
                e.kind(),
                format!("Cannot start the client's runtime: {e}"),
            ))
        })
}
