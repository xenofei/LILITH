//! One connection (ARCH-025, ARCH-033, QA-010): NDJSON framing and the order rules.
//!
//! - Lines are read by one `FramedRead` with a `LinesCodec` capped at `MAX_LINE_BYTES`. A
//!   writer task drains the outbound queue (`OUTBOUND_CAPACITY`), one line per message.
//! - Until the core has its router, a `core/prove` is answered ([`answer_prove`]) and the first
//!   other line waits for the router, unread lines behind it; a connection that ends before
//!   there is a router never reaches one.
//! - `Router::opened` runs first and `Router::closed` exactly once, at the end, also when a
//!   handler on the reader panicked (a drop guard; the panic then ends the connection).
//! - While the router reports no handshake, each request is awaited before the next line is
//!   read; afterwards each request runs in its own task, except those the router keeps on
//!   the reader (`Router::runs_inline`).
//! - A request task holds one of the connection's `MAX_IN_FLIGHT_PER_CONN` permits. With
//!   none free, the reader waits before it reads on: backpressure on the pipe, nothing is
//!   dropped. Waiting for a permit ends when `close` is cancelled, so a drain or shutdown never
//!   waits for a full connection. A request task that panics is answered with
//!   `Router::panicked`.
//! - A line that is not a request gets its protocol fault, and the connection stays open; one
//!   that is not UTF-8 gets -32700 with id `null`. A line of up to `MAX_LINE_BYTES` before its
//!   `\n` is read; a longer one gets -32002 with id `null`, and the connection then closes. A
//!   response is never written over the cap either: one that would be gets -32603 for its id
//!   instead ([`ConnCtx::send_response`]).
//!   Notifications are ignored; a partial line at end-of-stream is dropped.
//! - Cancelling `ConnCtx::close` ends the connection. Lines already queued by then (the reply
//!   to `core/drain` among them) are still written for up to `FLUSH_GRACE`.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use codec::Frame;
use futures_util::{FutureExt as _, StreamExt as _};
use lil_protocol::{Response, RpcError, method, parse_request, rpc_code};
use tokio::io::{AsyncWriteExt as _, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::codec::{FramedRead, LinesCodecError};
use tokio_util::sync::CancellationToken;

use super::{ConnCtx, LateRouter, MAX_IN_FLIGHT_PER_CONN, OUTBOUND_CAPACITY, Router, answer_prove};

/// How long a closed connection may still spend writing the lines queued before it closed.
const FLUSH_GRACE: Duration = Duration::from_secs(1);

/// A connection's lines.
type Lines = FramedRead<ReadHalf<NamedPipeServer>, codec::LineCodec>;
/// One read of [`Lines`].
type Read = Result<Frame, LinesCodecError>;

/// Serves one connected pipe instance until the client leaves, an over-long line arrives or
/// `close` is cancelled.
pub(super) async fn run(
    server: NamedPipeServer,
    conn_id: u64,
    client_pid: u32,
    mut router: LateRouter,
    close: CancellationToken,
) {
    let (read, write) = tokio::io::split(server);
    let (outbound, queue) = mpsc::channel(OUTBOUND_CAPACITY);
    let ctx = Arc::new(ConnCtx {
        conn_id,
        client_pid,
        outbound,
        close: close.clone(),
    });
    // Set when reading has ended: the writer sends what is queued, then stops.
    let finish = CancellationToken::new();
    let writer = tokio::spawn(write_lines(write, queue, close.clone(), finish.clone()));

    tracing::debug!(conn_id, client_pid, "connection opened");
    let mut lines = FramedRead::new(read, Frame::codec());
    let Some((router, first)) = until_router(&mut lines, &ctx, &mut router).await else {
        finish.cancel();
        let _ = writer.await;
        return;
    };
    router.opened(&ctx);
    {
        // Runs `closed` and lets the writer finish however reading ends, panics included.
        let _ending = Ending {
            router: &router,
            ctx: &ctx,
            finish: &finish,
        };
        read_lines(lines, first, &ctx, &router).await;
    }
    let _ = writer.await;
}

/// The end of a connection's reading: [`Router::closed`], then the writer sends what is
/// queued and stops. A drop guard, so it also runs when a handler on the reader panics.
struct Ending<'a> {
    router: &'a Arc<dyn Router>,
    ctx: &'a Arc<ConnCtx>,
    finish: &'a CancellationToken,
}

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(
                conn_id = self.ctx.conn_id,
                "a request handler on the connection's reader panicked; the connection ends"
            );
        }
        self.router.closed(self.ctx);
        tracing::debug!(conn_id = self.ctx.conn_id, "connection closed");
        self.finish.cancel();
    }
}

/// The router, once the core has it, and the line read meanwhile that waits for it. `None`: the
/// connection ended first. Meanwhile each `core/prove` is answered.
async fn until_router(
    lines: &mut Lines,
    ctx: &ConnCtx,
    late: &mut LateRouter,
) -> Option<(Arc<dyn Router>, Option<Read>)> {
    let mut first = None;
    loop {
        tokio::select! {
            biased;
            () = ctx.close.cancelled() => return None,
            router = the_router(late) => return Some((router?, first)),
            next = lines.next(), if first.is_none() => {
                let next = next?;
                let prove = match &next {
                    Ok(Frame::Line(line)) => parse_request(line)
                        .ok()
                        .filter(|req| req.method == method::PROVE),
                    _ => None,
                };
                match prove {
                    // A notification is nothing the core acts on.
                    Some(req) => {
                        if let Some(id) = req.id {
                            send(ctx, &answer_prove(id, req.params)).await;
                        }
                    }
                    None => first = Some(next),
                }
            }
        }
    }
}

/// The router, once it is there. `None`: the core gave up before it had one.
async fn the_router(late: &mut LateRouter) -> Option<Arc<dyn Router>> {
    late.wait_for(Option::is_some).await.ok()?.clone()
}

async fn read_lines(
    mut lines: Lines,
    mut first: Option<Read>,
    ctx: &Arc<ConnCtx>,
    router: &Arc<dyn Router>,
) {
    // Never closed, so acquiring fails only if it did.
    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_PER_CONN));
    loop {
        let next = match first.take() {
            Some(read) => Some(read),
            None => tokio::select! {
                biased;
                () = ctx.close.cancelled() => return,
                next = lines.next() => next,
            },
        };
        let line = match next {
            // End of stream; a partial last line has been dropped by the codec.
            None => return,
            Some(Ok(Frame::Line(line))) => line,
            Some(Ok(Frame::NotUtf8)) => {
                let error = RpcError::new(rpc_code::PARSE_ERROR, "parse error: not UTF-8");
                send(ctx, &Response::Error { id: None, error }).await;
                continue;
            }
            Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                let error = RpcError::new(rpc_code::LINE_TOO_LONG, "line too long");
                send(ctx, &Response::Error { id: None, error }).await;
                return;
            }
            Some(Err(LinesCodecError::Io(e))) => {
                tracing::debug!(conn_id = ctx.conn_id, error = %e, "the connection failed");
                return;
            }
        };
        let req = match parse_request(&line) {
            Ok(req) => req,
            Err((id, error)) => {
                send(ctx, &Response::Error { id, error }).await;
                continue;
            }
        };
        if req.id.is_none() {
            // Clients send no notifications the core acts on.
            continue;
        }
        if router.handshake_done(ctx) && !router.runs_inline(&req) {
            let permit = tokio::select! {
                biased;
                () = ctx.close.cancelled() => return,
                permit = permits.clone().acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(_) => return,
                },
            };
            let ctx = ctx.clone();
            let router = router.clone();
            tokio::spawn(async move {
                // Held until the response is queued (or the task ends some other way).
                let _permit = permit;
                let id = req.id.clone();
                let handled = AssertUnwindSafe(router.handle(&ctx, req))
                    .catch_unwind()
                    .await;
                let response = match handled {
                    Ok(response) => response,
                    Err(payload) => {
                        let text = payload
                            .downcast_ref::<&str>()
                            .map(|s| (*s).to_string())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "a panic without a message".to_string());
                        tracing::error!(conn_id = ctx.conn_id, panic = %text, "a request handler panicked");
                        id.map(|id| router.panicked(&ctx, id))
                    }
                };
                if let Some(response) = response {
                    send(&ctx, &response).await;
                }
            });
        } else {
            let response = tokio::select! {
                biased;
                () = ctx.close.cancelled() => return,
                response = router.handle(ctx, req) => response,
            };
            if let Some(response) = response {
                send(ctx, &response).await;
            }
        }
    }
}

/// Queues `response` for the writer, within the line cap ([`ConnCtx::send_response`]).
async fn send(ctx: &ConnCtx, response: &Response) {
    ctx.send_response(response).await;
}

/// Writes each queued line with its `\n`. Stops once the queue is empty after `close` or
/// `finish` is cancelled. After `close`, what is still queued may take at most `FLUSH_GRACE`
/// to write, so a client that does not read cannot hold the connection open.
async fn write_lines(
    mut write: WriteHalf<NamedPipeServer>,
    mut queue: mpsc::Receiver<String>,
    close: CancellationToken,
    finish: CancellationToken,
) {
    let out_of_grace = async {
        close.cancelled().await;
        tokio::time::sleep(FLUSH_GRACE).await;
    };
    tokio::pin!(out_of_grace);
    loop {
        let line = tokio::select! {
            biased;
            line = queue.recv() => match line {
                Some(line) => line,
                None => return,
            },
            // Reached only while the queue is empty (the branches are polled in order).
            () = close.cancelled() => return,
            () = finish.cancelled() => return,
        };
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        let written = tokio::select! {
            biased;
            () = &mut out_of_grace => return,
            written = write.write_all(&bytes) => written,
        };
        if written.is_err() {
            return;
        }
    }
}

/// The line codec: `LinesCodec` with the 4 MiB cap, with two changes. A line that is not
/// UTF-8 is a [`Frame::NotUtf8`] item instead of an error that would end the stream, and a
/// partial line at end-of-stream is dropped instead of returned.
mod codec {
    use lil_protocol::MAX_LINE_BYTES;
    use tokio_util::bytes::BytesMut;
    use tokio_util::codec::{Decoder, LinesCodec, LinesCodecError};

    pub(super) enum Frame {
        Line(String),
        NotUtf8,
    }

    impl Frame {
        pub(super) fn codec() -> LineCodec {
            LineCodec(LinesCodec::new_with_max_length(MAX_LINE_BYTES))
        }
    }

    pub(super) struct LineCodec(LinesCodec);

    impl Decoder for LineCodec {
        type Item = Frame;
        type Error = LinesCodecError;

        fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Frame>, LinesCodecError> {
            match self.0.decode(buf) {
                Ok(line) => Ok(line.map(Frame::Line)),
                // `LinesCodec` has consumed the line before it checked its encoding.
                Err(LinesCodecError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                    Ok(Some(Frame::NotUtf8))
                }
                Err(e) => Err(e),
            }
        }

        fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Frame>, LinesCodecError> {
            let frame = self.decode(buf)?;
            if frame.is_none() {
                buf.clear();
            }
            Ok(frame)
        }
    }
}
