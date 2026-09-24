//! One client connection: reading requests, answering them, and delivering events (0007).
//!
//! The reader parses frames and starts a task for each request, with at most
//! `max_requests_in_flight` at a time. Beyond that it stops reading, which pushes back on the
//! client. Every reply goes through a bounded queue to the writer. The writer empties that queue
//! before it writes any event, and it takes events from the event log through each
//! subscription's cursor rather than from a queue of its own, so a subscriber that lags reads from
//! the log until it catches up.
//!
//! When the client closes its side, or the server starts shutting down, the reader stops, and the
//! connection closes once every request it read has been answered. When the client is gone, has
//! been silent too long, or sends an oversized frame, the connection closes at once and its
//! requests are cancelled.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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
use crate::methods::{self, Context, Cursors, Reply, Session};

type InFlight = Arc<Mutex<HashMap<RequestId, CancellationToken>>>;

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
    let (replies, queue) = mpsc::channel(daemon.limits.outbound_queue);
    let reader = Reader {
        frames: FramedRead::new(read, FrameCodec::new()),
        permits: Arc::new(Semaphore::new(daemon.limits.max_requests_in_flight)),
        daemon: Arc::clone(&daemon),
        replies,
        session: None,
        in_flight: InFlight::default(),
        handlers: JoinSet::new(),
        stop_reading,
        closing: closing.clone(),
    };
    let writer = self::write(
        FramedWrite::new(write, FrameCodec::new()),
        queue,
        Arc::clone(&daemon.log),
        closing,
    );
    debug!("connected");
    let (session, ()) = tokio::join!(reader.run(), writer);
    if let Some(session) = session {
        info!(client = %session.client.name, protocol = session.protocol, "disconnected");
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
        let idle = self.daemon.limits.idle_timeout;
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
                debug!(id = ?response.id, "ignored a response; wispd sends no requests");
                true
            }
            Err(malformed) => {
                debug!(error = %malformed, "answered a malformed message");
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
                format!("Invalid request: request {} is still in flight", request.id),
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
        let span = debug_span!("request", id = %request.id, method = %request.method);
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
            debug!(method = %notification.method, "ignored a notification");
            return;
        }
        match notification.params::<<CancelRequest as NotificationMethod>::Params>() {
            Ok(CancelRequestParams { id }) => {
                if let Some(cancel) = lock(&self.in_flight).get(&id) {
                    debug!(%id, "cancelling a request");
                    cancel.cancel();
                } else {
                    debug!(%id, "ignored a cancel for a request that is not in flight");
                }
            }
            Err(error) => debug!(%error, "ignored a malformed $/cancelRequest"),
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

async fn write<W: AsyncWrite + Unpin>(
    sink: FramedWrite<W, FrameCodec>,
    queue: mpsc::Receiver<Reply>,
    log: Arc<EventLog>,
    closing: CancellationToken,
) {
    tokio::select! {
        biased;
        () = closing.cancelled() => {}
        written = write_all(sink, queue, &log) => {
            if let Err(error) = written {
                debug!(%error, "writing failed");
            }
        }
    }
    closing.cancel();
}

async fn write_all<W: AsyncWrite + Unpin>(
    mut sink: FramedWrite<W, FrameCodec>,
    mut queue: mpsc::Receiver<Reply>,
    log: &EventLog,
) -> Result<(), FrameError> {
    let mut appended = log.watch();
    let mut cursors = Cursors::default();
    loop {
        match queue.try_recv() {
            Ok(reply) => {
                send_reply(&mut sink, reply, &mut cursors).await?;
                continue;
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        match cursors.next(log) {
            Ok(Some(event)) => {
                send_event(&mut sink, event).await?;
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
        SinkExt::<Response>::flush(&mut sink).await?;
        tokio::select! {
            reply = queue.recv() => match reply {
                Some(reply) => send_reply(&mut sink, reply, &mut cursors).await?,
                None => break,
            },
            changed = appended.changed() => if changed.is_err() {
                break;
            },
        }
    }
    SinkExt::<Response>::close(&mut sink).await
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
            warn!(id = ?response.id, "a response was larger than the frame limit");
            let error = ErrorObject::internal_error(format!(
                "the result is larger than the {max_frame_bytes}-byte frame limit"
            ));
            sink.feed(Response::error(response.id, error)).await
        }
        sent => sent,
    }
}

async fn send_event<W: AsyncWrite + Unpin>(
    sink: &mut FramedWrite<W, FrameCodec>,
    event: EventsEventParams,
) -> Result<(), FrameError> {
    let seq = event.seq;
    match sink.feed(Notification::new::<EventsEvent>(event)).await {
        Err(FrameError::TooLarge { .. }) => {
            warn!(seq, "skipped an event larger than the frame limit");
            Ok(())
        }
        sent => sent,
    }
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
    use tokio_util::codec::FramedRead;
    use tokio_util::sync::CancellationToken;
    use wisp_protocol::framing::FrameCodec;
    use wisp_protocol::jsonrpc::Message;
    use wisp_protocol::{Project, ProjectId, WispEvent};

    use super::serve;
    use crate::event_log::EventLog;
    use crate::server::{Daemon, Limits};
    use crate::store::StoreHandle;

    const PATIENCE: Duration = Duration::from_secs(10);

    type Frames = FramedRead<ReadHalf<DuplexStream>, FrameCodec>;

    fn daemon(dir: &Path, retention: usize) -> Arc<Daemon> {
        Arc::new(Daemon {
            started: std::time::Instant::now(),
            log: Arc::new(EventLog::new(retention)),
            store: StoreHandle::open(&dir.join("wispd.sqlite3")),
            os: "test".to_owned(),
            limits: Limits {
                idle_timeout: Duration::from_secs(90),
                max_requests_in_flight: 32,
                outbound_queue: 32,
            },
        })
    }

    fn append(daemon: &Daemon, count: usize) {
        for _ in 0..count {
            let project = Project {
                id: ProjectId::generate(),
                name: "wisp".to_owned(),
                repo_path: "/src/wisp".to_owned(),
                created_at: Timestamp::now(),
                updated_at: Timestamp::now(),
            };
            daemon.log.append(
                Timestamp::now(),
                None,
                WispEvent::ProjectCreated { project },
            );
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
        append(&daemon, 5_000);
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
        append(&daemon, 1_000);

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
}
