//! The client for synchronous callers such as `lil` (ruling P11): no tokio in the caller.
//!
//! [`BlockingClient`] owns a current-thread runtime and runs each call on it, so its
//! connection's reader only runs while a call (or [`Events::next`]) is waiting; the pipe
//! buffers what arrives meanwhile. [`drain`] is `lil core stop`. None of it may be used from
//! inside a tokio runtime (`block_on` panics there). QA-026's GUI-thread check is deferred to
//! plan 4.

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use lil_protocol::{EventParams, HelloResult, Outcome, ToolDescriptor, ToolOutput, TopicState};
use tokio::runtime::Runtime;

use crate::{Client, ClientConfig, ClientError, DrainOutcome, EventStream, drain_and_wait};

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
        self.rt.block_on(self.client.call_tool(name, args, key))
    }

    /// [`Client::confirmations`].
    pub fn confirmations(
        &self,
        state: lil_protocol::app::ListWhich,
    ) -> Result<Outcome<lil_protocol::app::ConfirmationsListResult>, ClientError> {
        self.rt.block_on(self.client.confirmations(state))
    }

    /// [`Client::list_tools`].
    pub fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ClientError> {
        self.rt.block_on(self.client.list_tools())
    }

    /// [`Client::subscribe`], as a blocking iterator of events that ends when the core goes
    /// away (`lil events --follow`).
    pub fn subscribe(
        &self,
        topics: Vec<String>,
        since: BTreeMap<String, u64>,
    ) -> Result<Events<'_>, ClientError> {
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
        self.rt
            .block_on(async { tokio::time::timeout(within, self.stream.next()).await })
            .map_err(|_| ClientError::Timeout)
    }
}

impl Iterator for Events<'_> {
    type Item = EventParams;

    fn next(&mut self) -> Option<EventParams> {
        self.rt.block_on(self.stream.next())
    }
}

/// [`drain_and_wait`] on a runtime of its own: `lil core stop`.
pub fn drain(cfg: &ClientConfig, within: Duration) -> Result<DrainOutcome, ClientError> {
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
