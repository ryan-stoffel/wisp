//! One client connection: reading requests, answering them, and delivering events (0007).
//!
//! The reader parses frames and starts a task for each request, with at most
//! `MAX_REQUESTS_IN_FLIGHT` at a time. Beyond that it stops reading, which pushes back on the
//! client. Every reply goes through a bounded queue to the writer. The writer empties that queue
//! before it writes any event, and it takes events from the event log through each
//! subscription's cursor rather than from a queue of its own, so a subscriber that lags reads from
//! the log until it catches up.
//!
//! When the client closes its side, or the server starts shutting down, the reader stops, and the
//! connection closes once every request it read has been answered. When the client is gone, has
//! been silent too long, stops reading what wispd writes, or sends an oversized frame, the
//! connection closes at once and its requests are cancelled.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::{FutureExt, SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::{JoinError, JoinSet};
use tokio::time::{self, Instant};
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, debug_span, error, info, warn};
use wisp_protocol::framing::{FrameCodec, FrameError};
use wisp_protocol::jsonrpc::{
    CancelRequestParams, ErrorObject, INVALID_REQUEST, Message, Notification, Request, RequestId,
    Response,
};
use wisp_protocol::methods::{
    CancelRequest, EventsEvent, Initialize, NotificationMethod, RequestMethod,
};
use wisp_protocol::{ErrorKind, EventsEventParams};

use super::Daemon;
use crate::event_log::EventLog;
use crate::logging::{Untrusted, UntrustedId};
use crate::methods::{self, Context, Cursors, Reply, Session};

type InFlight = Arc<Mutex<HashMap<RequestId, CancellationToken>>>;

/// Requests one connection may have in flight before the server stops reading from it.
const MAX_REQUESTS_IN_FLIGHT: usize = 32;

/// Replies one connection may have waiting to be written.
const OUTBOUND_QUEUE: usize = 32;

/// Serves one connection until it closes. `closing` closes it at once; `stop_reading` stops
/// reading and closes it after the requests already read are answered.
pub(crate) async fn serve<S>(
    stream: S,
    daemon: Arc<Daemon>,
    stop_reading: CancellationToken,
    closing: CancellationToken,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read, write) = tokio::io::split(stream);
    let (replies, queue) = mpsc::channel(OUTBOUND_QUEUE);
    let reader = Reader {
        frames: FramedRead::new(read, FrameCodec::new()),
        permits: Arc::new(Semaphore::new(MAX_REQUESTS_IN_FLIGHT)),
        daemon: Arc::clone(&daemon),
        replies,
        session: None,
        in_flight: InFlight::default(),
        handlers: JoinSet::new(),
        stop_reading: stop_reading.clone(),
        closing: closing.clone(),
    };
    let stall = Stall {
        timeout: daemon.idle_timeout,
        stopping: stop_reading.clone(),
    };
    let writer = run_writer(
        FramedWrite::new(write, FrameCodec::new()),
        queue,
        Arc::clone(&daemon.log),
        closing,
        stall,
    );
    debug!("connected");
    let (session, ()) = tokio::join!(reader.run(), writer);
    if let Some(session) = session {
        info!(
            client = ?Untrusted(&session.client.name),
            protocol = session.protocol,
            "disconnected"
        );
    } else {
        debug!("disconnected before initialize");
    }
}

enum End {
    /// Answer what was read, then close.
    Drain,
    /// Close now and cancel what is in flight.
    Close,
}

struct Reader<S> {
    frames: FramedRead<ReadHalf<S>, FrameCodec>,
    daemon: Arc<Daemon>,
    replies: mpsc::Sender<Reply>,
    session: Option<Session>,
    in_flight: InFlight,
    handlers: JoinSet<()>,
    permits: Arc<Semaphore>,
    stop_reading: CancellationToken,
    closing: CancellationToken,
}

impl<S: AsyncRead + AsyncWrite + Send + 'static> Reader<S> {
    async fn run(mut self) -> Option<Session> {
        match self.read().await {
            End::Drain => self.drain().await,
            End::Close => self.closing.cancel(),
        }
        // Cancelling first means a store job still queued for an aborted handler is skipped.
        for cancel in lock(&self.in_flight).values() {
            cancel.cancel();
        }
        self.handlers.shutdown().await;
        self.session.take()
    }

    async fn read(&mut self) -> End {
        let idle = self.daemon.idle_timeout;
        let mut deadline = Instant::now() + idle;
        loop {
            let next = tokio::select! {
                biased;
                () = self.closing.cancelled() => return End::Close,
                () = self.stop_reading.cancelled() => return End::Drain,
                Some(joined) = self.handlers.join_next() => {
                    log_join(joined);
                    continue;
                }
                () = time::sleep_until(deadline) => {
                    info!(?idle, "closing a connection that has been silent too long");
                    return End::Close;
                }
                next = self.frames.next() => next,
            };
            deadline = Instant::now() + idle;
            let frame = match next {
                None => {
                    debug!("the client closed its side");
                    return End::Drain;
                }
                Some(Ok(frame)) => frame,
                Some(Err(FrameError::TooLarge { max_frame_bytes })) => {
                    warn!(
                        max_frame_bytes,
                        "closing a connection that sent an oversized frame"
                    );
                    return End::Close;
                }
                Some(Err(error)) => {
                    debug!(%error, "reading failed");
                    return End::Close;
                }
            };
            if !self.frame(&frame).await {
                return End::Close;
            }
        }
    }

    async fn drain(&mut self) {
        loop {
            tokio::select! {
                biased;
                () = self.closing.cancelled() => return,
                joined = self.handlers.join_next() => match joined {
                    Some(joined) => log_join(joined),
                    None => return,
                },
            }
        }
    }

    /// Handles one frame. Returns false when the connection has to close.
    async fn frame(&mut self, frame: &[u8]) -> bool {
        match Message::from_frame(frame) {
            Ok(Message::Request(request)) => self.request(request).await,
            Ok(Message::Notification(notification)) => {
                self.notification(&notification);
                true
            }
            Ok(Message::Response(response)) => {
                debug!(
                    id = ?response.id.as_ref().map(UntrustedId),
                    "ignored a response; wispd sends no requests"
                );
                true
            }
            Err(malformed) => {
                debug!(
                    error = ?Untrusted(&malformed.error.message),
                    "answered a malformed message"
                );
                self.reply(Reply::Response(malformed.into_response())).await
            }
        }
    }

    async fn request(&mut self, request: Request) -> bool {
        if self.session.is_none() {
            return self.before_initialize(request).await;
        }
        if lock(&self.in_flight).contains_key(&request.id) {
            let error = ErrorObject::new(
                INVALID_REQUEST,
                "Invalid request: a request with this id is still in flight",
            );
            let response = Response::error(Some(request.id), error);
            return self.reply(Reply::Response(response)).await;
        }
        let permit = tokio::select! {
            biased;
            () = self.closing.cancelled() => return false,
            permit = Arc::clone(&self.permits).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return false,
            },
        };
        let cancel = CancellationToken::new();
        lock(&self.in_flight).insert(request.id.clone(), cancel.clone());
        let context = Context {
            daemon: Arc::clone(&self.daemon),
            cancel,
        };
        let in_flight = Arc::clone(&self.in_flight);
        let replies = self.replies.clone();
        let span = debug_span!(
            "request",
            id = ?UntrustedId(&request.id),
            method = ?Untrusted(&request.method)
        );
        self.handlers.spawn(
            async move {
                let id = request.id.clone();
                let dispatched = AssertUnwindSafe(methods::dispatch(context, request))
                    .catch_unwind()
                    .await;
                let reply = dispatched.unwrap_or_else(|_| {
                    error!("the request's handler panicked");
                    let error = ErrorObject::internal_error("the request failed unexpectedly");
                    Reply::Response(Response::error(Some(id.clone()), error))
                });
                // Out of the map before the answer is queued, so a late cancel can't produce a
                // second answer.
                lock(&in_flight).remove(&id);
                debug!("answered");
                let _ = replies.send(reply).await;
                drop(permit);
            }
            .instrument(span),
        );
        true
    }

    // Requests before `initialize` run in order, right here, so a request pipelined behind
    // `initialize` already sees the connection initialized.
    async fn before_initialize(&mut self, request: Request) -> bool {
        let response = if request.method == Initialize::NAME {
            match methods::initialize(&self.daemon, &request) {
                Ok((session, result)) => {
                    self.session = Some(session);
                    methods::success(request.id, &result)
                }
                Err(error) => Response::error(Some(request.id), error),
            }
        } else {
            let error = ErrorObject::wisp(
                ErrorKind::NotInitialized,
                "initialize must be the first request",
            );
            Response::error(Some(request.id), error)
        };
        self.reply(Reply::Response(response)).await
    }

    fn notification(&self, notification: &Notification) {
        if notification.method != CancelRequest::NAME {
            debug!(
                method = ?Untrusted(&notification.method),
                "ignored a notification"
            );
            return;
        }
        match notification.params::<<CancelRequest as NotificationMethod>::Params>() {
            Ok(CancelRequestParams { id }) => {
                if let Some(cancel) = lock(&self.in_flight).get(&id) {
                    debug!(id = ?UntrustedId(&id), "cancelling a request");
                    cancel.cancel();
                } else {
                    debug!(
                        id = ?UntrustedId(&id),
                        "ignored a cancel for a request that is not in flight"
                    );
                }
            }
            Err(error) => debug!(
                error = ?Untrusted(&error.message),
                "ignored a malformed $/cancelRequest"
            ),
        }
    }

    async fn reply(&self, reply: Reply) -> bool {
        tokio::select! {
            biased;
            () = self.closing.cancelled() => false,
            sent = self.replies.send(reply) => sent.is_ok(),
        }
    }
}

fn log_join(joined: Result<(), JoinError>) {
    if let Err(error) = joined
        && error.is_panic()
    {
        error!("a request task panicked");
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn run_writer<W: AsyncWrite + Unpin>(
    sink: FramedWrite<W, FrameCodec>,
    queue: mpsc::Receiver<Reply>,
    log: Arc<EventLog>,
    closing: CancellationToken,
    stall: Stall,
) {
    tokio::select! {
        biased;
        () = closing.cancelled() => {}
        written = write_loop(sink, queue, &log, &stall) => {
            if let Err(error) = written {
                debug!(%error, "writing failed");
            }
        }
    }
    closing.cancel();
}

/// When a write that makes no progress gives up: after `timeout`, or [`SHUTDOWN_WRITE_TIMEOUT`]
/// after a shutdown starts. A client that stops reading is dropped rather than holding its
/// connection, and the shutdown, open forever.
struct Stall {
    timeout: Duration,
    stopping: CancellationToken,
}

/// How long a stalled write may still wait once a shutdown has started.
const SHUTDOWN_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

impl Stall {
    async fn guard<T>(
        &self,
        write: impl Future<Output = Result<T, FrameError>>,
    ) -> Result<T, FrameError> {
        let stalled = async {
            tokio::select! {
                () = time::sleep(self.timeout) => {}
                () = async {
                    self.stopping.cancelled().await;
                    time::sleep(SHUTDOWN_WRITE_TIMEOUT).await;
                } => {}
            }
        };
        tokio::select! {
            written = write => written,
            () = stalled => {
                info!("closing a connection whose client stopped reading");
                Err(FrameError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the client stopped reading",
                )))
            }
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut sink: FramedWrite<W, FrameCodec>,
    mut queue: mpsc::Receiver<Reply>,
    log: &EventLog,
    stall: &Stall,
) -> Result<(), FrameError> {
    let mut appended = log.watch();
    let mut cursors = Cursors::default();
    loop {
        match queue.try_recv() {
            Ok(reply) => {
                stall
                    .guard(send_reply(&mut sink, reply, &mut cursors))
                    .await?;
                continue;
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        match cursors.next(log) {
            Ok(Some(event)) => {
                stall.guard(send_event(&mut sink, event)).await?;
                // A long replay may never wait on the socket; let other tasks run.
                tokio::task::consume_budget().await;
                continue;
            }
            Ok(None) => {}
            Err(subscription) => {
                warn!(
                    %subscription,
                    "a subscriber fell behind the event log's retention; closing the connection so it resubscribes"
                );
                return Ok(());
            }
        }
        // FramedWrite is a Sink for every serializable item, so flush and close name one.
        stall.guard(SinkExt::<Response>::flush(&mut sink)).await?;
        tokio::select! {
            reply = queue.recv() => match reply {
                Some(reply) => {
                    stall
                        .guard(send_reply(&mut sink, reply, &mut cursors))
                        .await?;
                }
                None => break,
            },
            changed = appended.changed() => if changed.is_err() {
                break;
            },
        }
    }
    stall.guard(SinkExt::<Response>::close(&mut sink)).await
}

async fn send_reply<W: AsyncWrite + Unpin>(
    sink: &mut FramedWrite<W, FrameCodec>,
    reply: Reply,
    cursors: &mut Cursors,
) -> Result<(), FrameError> {
    match reply {
        Reply::Response(response) => send_response(sink, response).await,
        Reply::Subscribe { response, cursor } => {
            send_response(sink, response).await?;
            cursors.add(cursor);
            Ok(())
        }
        Reply::Unsubscribe {
            response,
            subscription,
        } => {
            cursors.remove(subscription);
            send_response(sink, response).await
        }
    }
}

async fn send_response<W: AsyncWrite + Unpin>(
    sink: &mut FramedWrite<W, FrameCodec>,
    response: Response,
) -> Result<(), FrameError> {
    match sink.feed(&response).await {
        Err(FrameError::TooLarge { max_frame_bytes }) => {
            warn!(
                id = ?response.id.as_ref().map(UntrustedId),
                "a response was larger than the frame limit"
            );
            let error = ErrorObject::internal_error(format!(
                "the result is larger than the {max_frame_bytes}-byte frame limit"
            ));
            sink.feed(Response::error(response.id, error)).await
        }
        sent => sent,
    }
}

// An event too large for a frame fails the connection rather than being skipped, so a subscriber
// never goes on with a gap it can't see. Project fields have size limits, which keep M1's events
// far below the frame limit.
async fn send_event<W: AsyncWrite + Unpin>(
    sink: &mut FramedWrite<W, FrameCodec>,
    event: EventsEventParams,
) -> Result<(), FrameError> {
    let seq = event.seq;
    let sent = sink.feed(Notification::new::<EventsEvent>(event)).await;
    if let Err(FrameError::TooLarge { max_frame_bytes }) = &sent {
        error!(
            seq,
            max_frame_bytes, "an event is larger than the frame limit; closing the connection"
        );
    }
    sent
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::StreamExt;
    use jiff::Timestamp;
    use serde_json::{Value, json};
    use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
    use tokio::time::timeout;
    use tokio_util::codec::{FramedRead, FramedWrite};
    use tokio_util::sync::CancellationToken;
    use wisp_protocol::framing::{FrameCodec, FrameError};
    use wisp_protocol::jsonrpc::Message;
    use wisp_protocol::{EventsEventParams, Project, ProjectId, SubscriptionId, WispEvent};

    use super::{send_event, serve};
    use crate::server::Daemon;

    const PATIENCE: Duration = Duration::from_secs(10);

    type Frames = FramedRead<ReadHalf<DuplexStream>, FrameCodec>;

    fn daemon(dir: &Path, retention: usize) -> Arc<Daemon> {
        Daemon::for_tests(dir, retention, Duration::from_secs(90))
    }

    async fn append(daemon: &Daemon, count: usize) {
        for _ in 0..count {
            let project = Project {
                id: ProjectId::generate(),
                name: "wisp".to_owned(),
                repo_path: "/src/wisp".to_owned(),
                branch: None,
                created_at: Timestamp::now(),
                updated_at: Timestamp::now(),
            };
            daemon
                .log
                .append(
                    Timestamp::now(),
                    None,
                    WispEvent::ProjectCreated { project },
                )
                .await;
        }
    }

    // The pipe holds only 1 KiB each way, so the server's writes back up quickly. The write half
    // is returned so the connection stays open, as a live editor's does.
    async fn connect(daemon: Arc<Daemon>, requests: &[Value]) -> (Frames, WriteHalf<DuplexStream>) {
        let (client, server) = tokio::io::duplex(1024);
        tokio::spawn(serve(
            server,
            daemon,
            CancellationToken::new(),
            CancellationToken::new(),
        ));
        let (read, mut write) = tokio::io::split(client);
        for request in requests {
            let mut line = serde_json::to_vec(request).unwrap();
            line.push(b'\n');
            write.write_all(&line).await.unwrap();
        }
        (FramedRead::new(read, FrameCodec::new()), write)
    }

    fn requests(after: u64) -> [Value; 3] {
        [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocol": {"min": 1, "max": 1},
                "client": {"name": "test", "version": "0"},
                "capabilities": {}
            }}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "events/subscribe", "params": {"after": after}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "host/health"}),
        ]
    }

    async fn next(frames: &mut Frames) -> Option<Message> {
        let frame = timeout(PATIENCE, frames.next())
            .await
            .expect("a message or the end")?;
        Some(Message::from_frame(&frame.ok()?).unwrap())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_answer_goes_out_ahead_of_a_long_replay() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = daemon(dir.path(), 10_000);
        append(&daemon, 5_000).await;
        let (mut frames, _write) = connect(daemon, &requests(0)).await;

        let mut events_before_health = 0;
        loop {
            match next(&mut frames).await.expect("the connection stays open") {
                Message::Notification(_) => events_before_health += 1,
                Message::Response(response) if response.id == Some(3.into()) => break,
                Message::Response(response) => assert!(response.result.is_ok()),
                Message::Request(request) => panic!("unexpected {request:?}"),
            }
        }
        assert!(
            events_before_health < 100,
            "{events_before_health} of 5000 replayed events went out before the answer"
        );
    }

    #[tokio::test]
    async fn a_subscriber_that_falls_out_of_the_retention_is_disconnected() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = daemon(dir.path(), 5);
        let (mut frames, _write) = connect(Arc::clone(&daemon), &requests(0)).await;
        for _ in 0..3 {
            assert!(matches!(
                next(&mut frames).await,
                Some(Message::Response(_))
            ));
        }
        append(&daemon, 1_000).await;

        let mut delivered = 0;
        while let Some(message) = next(&mut frames).await {
            assert!(matches!(message, Message::Notification(_)));
            delivered += 1;
        }
        assert!(
            delivered < 1_000,
            "{delivered} events, then the connection should close"
        );
    }

    #[tokio::test]
    async fn a_client_that_stops_reading_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let idle = Duration::from_millis(300);
        let daemon = Daemon::for_tests(dir.path(), 10, idle);
        let (client, server) = tokio::io::duplex(1024);
        tokio::spawn(serve(
            server,
            daemon,
            CancellationToken::new(),
            CancellationToken::new(),
        ));
        // The read half stays open and unread, so wispd's writes back up instead of failing.
        let (_unread, mut write) = tokio::io::split(client);
        let started = std::time::Instant::now();
        let flood = async move {
            let mut initialize = serde_json::to_vec(&requests(0)[0]).unwrap();
            initialize.push(b'\n');
            write.write_all(&initialize).await?;
            for id in 2_u64.. {
                let line =
                    format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"host/health\"}}\n");
                write.write_all(line.as_bytes()).await?;
            }
            Ok::<(), std::io::Error>(())
        };
        let flooded = timeout(PATIENCE, flood)
            .await
            .expect("wispd drops the connection instead of waiting forever");
        assert!(flooded.is_err(), "writes fail once wispd has closed");
        assert!(started.elapsed() >= idle, "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn an_event_too_large_for_a_frame_is_an_error_not_a_gap() {
        let mut sink = FramedWrite::new(tokio::io::sink(), FrameCodec::with_max_frame_bytes(1024));
        let event = |name: &str| EventsEventParams {
            subscription: SubscriptionId::generate(),
            seq: 1,
            time: Timestamp::now(),
            project: None,
            event: WispEvent::ProjectCreated {
                project: Project {
                    id: ProjectId::generate(),
                    name: name.to_owned(),
                    repo_path: "/src".to_owned(),
                    branch: None,
                    created_at: Timestamp::now(),
                    updated_at: Timestamp::now(),
                },
            },
        };
        assert!(send_event(&mut sink, event("wisp")).await.is_ok());
        assert!(matches!(
            send_event(&mut sink, event(&"n".repeat(2000))).await,
            Err(FrameError::TooLarge { .. })
        ));
    }
}
