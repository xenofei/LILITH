//! One connection to the core (ARCH-025, ARCH-032): NDJSON framing, pipelined calls and event
//! routing.
//!
//! - A writer task owns the pipe's write half and writes whole lines from a queue, so a call
//!   whose future is dropped never leaves half a line behind.
//! - A reader task owns the read half. Each response goes to the call waiting for its id;
//!   each `ww/event` notification goes, at any time, to every [`EventStream`] whose topics
//!   include its topic. One reader keeps the core's order, so no event is lost or reordered.
//! - A subscription's stream is registered *before* its `ww/subscribe` request is written:
//!   replayed events may arrive before the reply (the core queues them while it subscribes),
//!   and they are buffered in the stream until it is read.
//! - When either task ends (the core closed the pipe, a read or write failed, a line was
//!   over-long, not UTF-8 or not a server line), the connection is closed: waiting calls get [`ClientError::Closed`] and every stream ends after its
//!   buffered events.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures_util::StreamExt as _;
use serde_json::Value;
use tokio::io::{AsyncWriteExt as _, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, LinesCodec};
use ww_protocol::{
    EventParams, Id, MAX_LINE_BYTES, Response, RpcError, ServerLine, TopicState, encode_request,
    method, parse_server_line, rpc_code,
};

use crate::ClientError;

/// Lines waiting for the writer.
const OUTBOUND_CAPACITY: usize = 64;

type Reply = Result<Value, RpcError>;

/// A live connection. Dropping the last reference closes the pipe.
pub(crate) struct Conn {
    outbound: mpsc::Sender<String>,
    shared: Arc<Shared>,
    next_id: AtomicI64,
    reader: JoinHandle<()>,
}

#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    pending: HashMap<i64, oneshot::Sender<Reply>>,
    sinks: Vec<Sink>,
    next_sink: u64,
    closed: bool,
}

/// Where one [`EventStream`]'s events go.
struct Sink {
    id: u64,
    topics: BTreeSet<String>,
    tx: mpsc::UnboundedSender<EventParams>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Every update is a single step, so the state stays consistent after a panic.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ends the connection: waiting calls see `Closed`, streams end after what they hold.
    fn close(&self) {
        let mut inner = self.lock();
        inner.closed = true;
        inner.pending.clear();
        inner.sinks.clear();
    }
}

impl Conn {
    /// Starts the reader and writer tasks for `pipe` on the current runtime.
    pub(crate) fn start(pipe: NamedPipeClient) -> Arc<Conn> {
        let (read, write) = tokio::io::split(pipe);
        let shared = Arc::new(Shared::default());
        let (outbound, queue) = mpsc::channel(OUTBOUND_CAPACITY);
        tokio::spawn(write_lines(write, queue, shared.clone()));
        let reader = tokio::spawn(read_lines(read, shared.clone()));
        Arc::new(Conn {
            outbound,
            shared,
            next_id: AtomicI64::new(1),
            reader,
        })
    }

    /// Sends a request and waits for its response: the `result`, or the JSON-RPC error as
    /// [`ClientError::Protocol`]. Cancellation-safe: a dropped call forgets its id.
    pub(crate) async fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut line = encode_request(&Id::Num(id), method, &params);
        if line.len() > MAX_LINE_BYTES {
            return Err(ClientError::Protocol(RpcError::new(
                rpc_code::LINE_TOO_LONG,
                format!("the {method} request is longer than the 4 MiB line limit"),
            )));
        }
        line.push('\n');
        let (tx, rx) = oneshot::channel();
        {
            let mut inner = self.shared.lock();
            if inner.closed {
                return Err(ClientError::Closed);
            }
            inner.pending.insert(id, tx);
        }
        let _forget = Forget {
            shared: &self.shared,
            id,
        };
        self.outbound
            .send(line)
            .await
            .map_err(|_| ClientError::Closed)?;
        match rx.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(ClientError::Protocol(error)),
            Err(_) => Err(ClientError::Closed),
        }
    }

    /// Registers a stream for `topics`; `None` when the connection is closed.
    pub(crate) fn add_sink(
        &self,
        topics: &[String],
    ) -> Option<(u64, mpsc::UnboundedReceiver<EventParams>)> {
        let mut inner = self.shared.lock();
        if inner.closed {
            return None;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let id = inner.next_sink;
        inner.next_sink += 1;
        inner.sinks.push(Sink {
            id,
            topics: topics.iter().cloned().collect(),
            tx,
        });
        Some((id, rx))
    }

    pub(crate) fn remove_sink(&self, id: u64) {
        self.shared.lock().sinks.retain(|s| s.id != id);
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        // The writer ends with the queue's sender; the reader is stopped here, which closes the
        // read half (and with the write half, the pipe).
        self.reader.abort();
        self.shared.close();
    }
}

/// Removes a call's pending entry however the call ends.
struct Forget<'a> {
    shared: &'a Shared,
    id: i64,
}

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        self.shared.lock().pending.remove(&self.id);
    }
}

async fn write_lines(
    mut write: WriteHalf<NamedPipeClient>,
    mut queue: mpsc::Receiver<String>,
    shared: Arc<Shared>,
) {
    while let Some(line) = queue.recv().await {
        if write.write_all(line.as_bytes()).await.is_err() {
            shared.close();
            return;
        }
    }
}

async fn read_lines(read: ReadHalf<NamedPipeClient>, shared: Arc<Shared>) {
    let mut lines = FramedRead::new(read, LinesCodec::new_with_max_length(MAX_LINE_BYTES));
    // End of stream, a read error, an over-long line or bytes that are not UTF-8 all end the
    // connection: none of them is the protocol.
    while let Some(Ok(line)) = lines.next().await {
        if !route(&shared, &line) {
            break;
        }
    }
    shared.close();
}

/// Delivers one line from the core. `false`: the line is not a server line (not JSON, not
/// JSON-RPC 2.0), so the connection is no longer the protocol and must close; a reply the
/// line might have carried would otherwise leave its call waiting forever.
fn route(shared: &Shared, line: &str) -> bool {
    match parse_server_line(line) {
        Ok(ServerLine::Response(Response::Result {
            id: Id::Num(id),
            result,
        })) => reply(shared, id, Ok(result)),
        Ok(ServerLine::Response(Response::Error {
            id: Some(Id::Num(id)),
            error,
        })) => reply(shared, id, Err(error)),
        Ok(ServerLine::Notification(note)) if note.method == method::EVENT => {
            let Some(event) = note
                .params
                .and_then(|p| serde_json::from_value::<EventParams>(p).ok())
            else {
                return true;
            };
            // A stream whose receiver is gone is dropped here.
            shared.lock().sinks.retain(|sink| {
                !sink.topics.contains(&event.topic) || sink.tx.send(event.clone()).is_ok()
            });
        }
        Err(_) => return false,
        // A reply to nothing this client asked (a string id, or one already given up), a
        // fault without an id or another notification: nothing to deliver.
        Ok(_) => {}
    }
    true
}

fn reply(shared: &Shared, id: i64, reply: Reply) {
    let waiting = shared.lock().pending.remove(&id);
    if let Some(tx) = waiting {
        let _ = tx.send(reply);
    }
}

/// The events of one subscription, in the core's order. Replayed events come first when a
/// cursor was given. It keeps its connection open while it lives.
pub struct EventStream {
    rx: mpsc::UnboundedReceiver<EventParams>,
    topics: Vec<TopicState>,
    sink: u64,
    conn: Arc<Conn>,
}

impl EventStream {
    pub(crate) fn new(
        rx: mpsc::UnboundedReceiver<EventParams>,
        topics: Vec<TopicState>,
        sink: u64,
        conn: Arc<Conn>,
    ) -> EventStream {
        EventStream {
            rx,
            topics,
            sink,
            conn,
        }
    }

    /// The next event; `None` once the core went away and every buffered event was read.
    pub async fn next(&mut self) -> Option<EventParams> {
        self.rx.recv().await
    }

    /// Each subscribed topic with its sequence number when the subscription was made.
    pub fn topics(&self) -> &[TopicState] {
        &self.topics
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.conn.remove_sink(self.sink);
    }
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream")
            .field("topics", &self.topics)
            .finish_non_exhaustive()
    }
}
