//! One connection (ARCH-025, ARCH-033, QA-010): NDJSON framing and the order rules.
//!
//! - Lines are read by one `FramedRead` with a `LinesCodec` capped at `MAX_LINE_BYTES`. A
//!   writer task drains the outbound queue (`OUTBOUND_CAPACITY`), one line per message.
//! - `Router::opened` runs first and `Router::closed` exactly once, at the end.
//! - While the router reports no handshake, each request is awaited before the next line is
//!   read; afterwards each request runs in its own task.
//! - A line that is not a request gets its protocol fault. An over-long line gets -32002 with
//!   id `null`, and the connection then closes. Notifications are ignored; a partial line at
//!   end-of-stream is dropped.
//! - Cancelling `ConnCtx::close` ends the connection. Lines already queued by then (the reply
//!   to `core/drain` among them) are still written for up to `FLUSH_GRACE`.

use std::sync::Arc;
use std::time::Duration;

use codec::Frame;
use futures_util::StreamExt as _;
use tokio::io::{AsyncWriteExt as _, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodecError};
use tokio_util::sync::CancellationToken;
use ww_protocol::{Response, RpcError, encode_response, parse_request, rpc_code};

use super::{ConnCtx, OUTBOUND_CAPACITY, Router};

/// How long a closed connection may still spend writing the lines queued before it closed.
const FLUSH_GRACE: Duration = Duration::from_secs(1);

/// Serves one connected pipe instance until the client leaves, an over-long line arrives or
/// `close` is cancelled.
pub(super) async fn run(
    server: NamedPipeServer,
    conn_id: u64,
    client_pid: u32,
    router: Arc<dyn Router>,
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
    router.opened(&ctx);
    read_lines(read, &ctx, &router).await;
    router.closed(&ctx);
    tracing::debug!(conn_id, "connection closed");

    finish.cancel();
    let _ = writer.await;
}

async fn read_lines(read: ReadHalf<NamedPipeServer>, ctx: &Arc<ConnCtx>, router: &Arc<dyn Router>) {
    let mut lines = FramedRead::new(read, Frame::codec());
    loop {
        let next = tokio::select! {
            biased;
            () = ctx.close.cancelled() => return,
            next = lines.next() => next,
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
        if router.handshake_done(ctx) {
            let ctx = ctx.clone();
            let router = router.clone();
            tokio::spawn(async move {
                if let Some(response) = router.handle(&ctx, req).await {
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

/// Queues `response` for the writer. A connection whose writer has stopped drops it.
async fn send(ctx: &ConnCtx, response: &Response) {
    let _ = ctx.outbound.send(encode_response(response)).await;
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
    use tokio_util::bytes::BytesMut;
    use tokio_util::codec::{Decoder, LinesCodec, LinesCodecError};
    use ww_protocol::MAX_LINE_BYTES;

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
