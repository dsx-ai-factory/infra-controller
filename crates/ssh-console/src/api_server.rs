/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use arc_swap::ArcSwap;
use carbide_instrument::{Event, LabelValue};
use futures::Stream;
use rpc::protos::console_log::console_log_service_server::{
    ConsoleLogService, ConsoleLogServiceServer,
};
use rpc::protos::console_log::{ConsoleLogLine, StreamConsoleLogsRequest};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{RootCertStore, ServerConfig};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::{CancellationToken, DropGuard};
use tonic::transport::Server;
use tonic::transport::server::Connected;
use tonic::{Request, Response, Status};

use crate::bmc::client_pool::BmcConnectionStore;
use crate::config::Config;
use crate::console_logger::{self, LoggerHealth};
use crate::fork_cancel_token;
use crate::shutdown_handle::ShutdownHandle;

const DEFAULT_TAIL_LINES: usize = 1000;
const MAX_TAIL_LINES: usize = 1000;
/// Channel for giving messages to a live subscriber. Capacity represents how many messages to keep
/// in memory in case the subscriber is slow to consume them.
const LIVE_QUEUE_CAPACITY: usize = 4096;
/// Channel for sending responses to the client. It can stay small because it represents data we're
/// actually sending to the client (we block if it's full.) We're using LIVE_QUEUE_CAPACITY to store
/// message the client hasn't received yet, and it has the larger capacity.
const RESPONSE_QUEUE_CAPACITY: usize = 32;
const GAP_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const TLS_REFRESH_INTERVAL: Duration = Duration::from_mins(5);
const TLS_REFRESH_RETRY_DELAY: Duration = Duration::from_secs(15);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

type ResponseStream = Pin<Box<dyn Stream<Item = Result<ConsoleLogLine, Status>> + Send + 'static>>;

#[derive(Clone)]
struct Service {
    connections: BmcConnectionStore,
    allowed_spiffe_id: Arc<str>,
    cancel_token: CancellationToken,
}

#[derive(Clone)]
struct PeerIdentity(Option<Arc<str>>);

#[derive(Clone, Copy, LabelValue)]
enum StreamDropReason {
    ClientQueueFull,
    CompletedLineBroadcastLag,
}

#[derive(Event)]
#[event(
    event_name = "ssh_console_stream_lines_dropped",
    metric_name = "carbide_ssh_console_stream_lines_dropped_total",
    component = "ssh-console",
    log = off,
    metric = counter,
    message = "console lines were omitted from one client stream",
    describe = "Number of console lines omitted from client-specific streams, by reason"
)]
struct StreamLinesDropped {
    #[label]
    reason: StreamDropReason,
}

struct AuthorizedTlsStream {
    inner: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    identity: PeerIdentity,
}

impl Connected for AuthorizedTlsStream {
    type ConnectInfo = PeerIdentity;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.identity.clone()
    }
}

impl AsyncRead for AuthorizedTlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for AuthorizedTlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[tonic::async_trait]
impl ConsoleLogService for Service {
    type StreamConsoleLogsStream = ResponseStream;

    async fn stream_console_logs(
        &self,
        request: Request<StreamConsoleLogsRequest>,
    ) -> Result<Response<Self::StreamConsoleLogsStream>, Status> {
        let request = request.into_authorized_inner_request(&self.allowed_spiffe_id)?;
        let machine_id = request
            .machine_id
            .ok_or_else(|| Status::invalid_argument("machine_id is required"))?;
        let tail_lines = request.tail_lines()?;
        let access = self
            .connections
            .console_log_client(&machine_id)
            .ok_or_else(|| Status::not_found("machine is not present in the BMC pool"))?
            .ok_or_else(|| Status::failed_precondition("console logging is disabled"))?;

        // The strategy to seamlessly emit prior logs while not missing any new ones:
        //
        // Create two queues for receiving live logs:
        // - `live_rx`: The actual messages sent from the logger's broadcast channel, starting now.
        // - `client_live_rx`: Stores the messages while we wait for the client to receive them, with
        //   a processing step to avoid duplicates and to inform the client if it has lagged and
        //   missed any.
        let live_rx = access.subscribe()?;
        let (client_live_tx, client_live_rx) = mpsc::channel(LIVE_QUEUE_CAPACITY);

        // Then, get a snapshot of old log data to be sent.
        let snapshot = access.snapshot().await?;

        // Then, start a message pump which takes messages from `live_tx` and enqueues them into
        // client_live_tx`. This exists for three reasons:
        //
        // - To keep the broadcast channels clear (dequeueing messages right away) so that the log
        //   broadcast channel doesn't fill up
        // - To discard any messages that are already contained in the snapshot (ie. the sequence is
        //   less than snapshot.watermark)
        // - To inform the client if it has lagged and missed any.
        tokio::spawn(pump_live(
            live_rx,
            client_live_tx,
            snapshot.watermark,
            self.cancel_token.clone(),
            access,
        ));

        // Finally, this task will gather all from `snapshot` and send it to `response_tx`, and then
        // it will continually pump messages from `client_live_rx` into the response channel. This
        // ensures prior log lines are emitted first, while live messages queue up in
        // client_live_rx` until the full snapshot is sent.
        let (response_tx, response_rx) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
        tokio::spawn(send_all_to_response(
            snapshot,
            tail_lines,
            client_live_rx,
            response_tx,
            self.cancel_token.clone(),
        ));

        Ok(Response::new(Box::pin(ReceiverStream::new(response_rx))))
    }
}

trait ConsoleLogRequestHelper {
    fn tail_lines(&self) -> Result<usize, Status>;
}

impl ConsoleLogRequestHelper for StreamConsoleLogsRequest {
    fn tail_lines(&self) -> Result<usize, Status> {
        match self.tail_lines as usize {
            0 => Ok(DEFAULT_TAIL_LINES),
            n if n <= MAX_TAIL_LINES => Ok(n),
            _ => Err(Status::invalid_argument("tail_lines must not exceed 1000")),
        }
    }
}

trait RequestAuthorizer {
    type InnerRequest;
    fn authorize(&self, allowed: &str) -> Result<(), Status>;
    fn into_authorized_inner_request(self, allowed: &str) -> Result<Self::InnerRequest, Status>;
}

impl<T> RequestAuthorizer for Request<T> {
    type InnerRequest = T;

    fn authorize(&self, allowed: &str) -> Result<(), Status> {
        let actual = self
            .extensions()
            .get::<PeerIdentity>()
            .and_then(|identity| identity.0.as_deref())
            .ok_or_else(|| {
                Status::unauthenticated("client certificate has no valid SPIFFE identity")
            })?;
        if actual != allowed {
            return Err(Status::permission_denied(
                "client SPIFFE identity is not allowed",
            ));
        }
        Ok(())
    }

    fn into_authorized_inner_request(self, allowed: &str) -> Result<Self::InnerRequest, Status> {
        self.authorize(allowed)?;
        Ok(self.into_inner())
    }
}

impl From<LoggerHealth> for tonic::Status {
    fn from(value: LoggerHealth) -> Self {
        match value {
            LoggerHealth::Failed(message) => Status::internal(message.to_string()),
            LoggerHealth::Starting => Status::failed_precondition("console logger is starting"),
            LoggerHealth::Healthy => Status::failed_precondition("console logger is unavailable"),
            LoggerHealth::Stopped => Status::failed_precondition("console logger has stopped"),
        }
    }
}

/// Continually "pump" messages from the logger itself (`source`) to the `destination` receiver.
///
/// Start pumping messages once we get a message tagged with at least `watermark`, which is an
/// index into the logs. Clients first get a snapshot of old logs, then start at `watermark` to
/// continue them.
///
/// If the receiver is falling behind (for instance, if it's still receiving the initial "snapshot"
/// of logs, or if it's a slow connection), this manages the queue overflow behavior, keeping track
/// of how many messages were lost, and informing the receiver via a warning message that some log
/// lines were dropped.
async fn pump_live(
    mut source: broadcast::Receiver<Arc<console_logger::PublishedLine>>,
    destination: mpsc::Sender<Result<Arc<[u8]>, Status>>,
    watermark: u64,
    cancel_token: CancellationToken,
    access: console_logger::ConsoleLogClient,
) {
    let mut expected = watermark.saturating_add(1);
    let mut pending_drops = 0_u64;
    let mut interval = tokio::time::interval(GAP_RETRY_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while !cancel_token.is_cancelled() {
        let received = tokio::select! {
            _ = interval.tick(), if pending_drops > 0 => {
                if !try_send_gap(&destination, &mut pending_drops) && destination.is_closed() {
                    break;
                }
                continue;
            }
            _ = cancel_token.cancelled() => break,
            received = source.recv() => received,
        };

        let line = match received {
            Ok(line) => line,
            // Sequence numbers determine how many post-watermark lines were
            // lost; `count` can also include pre-watermark entries.
            Err(RecvError::Lagged(_count)) => {
                continue;
            }
            Err(RecvError::Closed) => {
                destination.send(Err(access.health().into())).await.ok();
                break;
            }
        };

        if line.sequence <= watermark {
            // We got lines that are already in the file we're sending as a snapshot, ignore them.
            continue;
        }

        if line.sequence > expected {
            // The pump itself is not keeping up with the producer, which should hopefully never
            // happen (we should be consuming from the broadcast channel without any blocking.)
            let count = line.sequence - expected;
            pending_drops += count;
            let reason = StreamDropReason::CompletedLineBroadcastLag;
            carbide_instrument::emit_count(StreamLinesDropped { reason }, count);
        }

        expected = line.sequence.saturating_add(1);

        if pending_drops > 0 && !try_send_gap(&destination, &mut pending_drops) {
            // We tried to inform the client there's a gap in the logs, but it didn't send ...
            if destination.is_closed() {
                // ... because it disconnected
                break;
            }
            // ... because it's too slow
            pending_drops += 1;
            let reason = StreamDropReason::ClientQueueFull;
            carbide_instrument::emit_count(StreamLinesDropped { reason }, 1);
            continue;
        }
        match destination.try_send(Ok(line.data.clone())) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                // The queue is full, the receiver is not picking up messages fast enough
                pending_drops += 1;
                let reason = StreamDropReason::ClientQueueFull;
                carbide_instrument::emit_count(StreamLinesDropped { reason }, 1);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => break,
        }
    }
}

/// Inform the receiver that there is a gap in the logs due to it consuming messages too slowly.
fn try_send_gap(
    destination: &mpsc::Sender<Result<Arc<[u8]>, Status>>,
    pending_drops: &mut u64,
) -> bool {
    let marker: Arc<[u8]> = Arc::from(
        format!(
            "[console-log-stream: {} lines omitted for this client]\n",
            *pending_drops
        )
        .into_bytes(),
    );
    match destination.try_send(Ok(marker)) {
        Ok(()) => {
            *pending_drops = 0;
            true
        }
        Err(TrySendError::Full(_)) => false,
        Err(TrySendError::Closed(_)) => false,
    }
}

/// Send logs to the client at `response_tx`, starting with the `snapshot`, then continuing from the
/// live log stream (`live_rx`).
///
/// While we're gathering the snapshot, the channel behind `live_rx` is gathering additional log
/// lines that have come in since we took the snapshot. After sending the snapshot to `response_tx`
/// we then continue with live logs. If fetching the snapshot takes too long, `live_rx`'s channel
/// will fill up, and the stream will include messages indicating how many log lines were dropped.
async fn send_all_to_response(
    snapshot: console_logger::Snapshot,
    tail_lines: usize,
    mut live_rx: mpsc::Receiver<Result<Arc<[u8]>, Status>>,
    response_tx: mpsc::Sender<Result<ConsoleLogLine, Status>>,
    cancel_token: CancellationToken,
) {
    // Get prior log lines, cancelling early if the client goes away or if we're cancelled.
    let history_result = tokio::select! {
        _ = response_tx.closed() => return,
        _ = cancel_token.cancelled() => return,
        result = snapshot.into_log_tail(tail_lines) => result,
    };
    let history = match history_result {
        Ok(lines) => lines,
        Err(error) => {
            let _ = response_tx
                .send(Err(Status::internal(error.to_string())))
                .await;
            return;
        }
    };

    // Send the prior log lines to the client first
    for data in history {
        if response_tx
            .send(Ok(ConsoleLogLine {
                data: data.to_vec(),
            }))
            .await
            .is_err()
        {
            return;
        }
    }

    // Then stream from `live_rx` to the client until we're cancelled or if the receiver goes away
    loop {
        let item = tokio::select! {
            _ = cancel_token.cancelled() => break,
            _ = response_tx.closed() => break,
            res = live_rx.recv() => match res {
                Some(res) => res,
                None => break,
            },
        };

        let item = item.map(|data| ConsoleLogLine {
            data: data.to_vec(),
        });
        if response_tx.send(item).await.is_err() {
            return;
        }
    }
}

pub(crate) struct Handle {
    listen_address: SocketAddr,
    join_handle: JoinHandle<()>,
    drop_guard: DropGuard,
}

impl Handle {
    pub(crate) fn listen_address(&self) -> SocketAddr {
        self.listen_address
    }
}

impl ShutdownHandle<()> for Handle {
    fn into_parts(self) -> (DropGuard, JoinHandle<()>) {
        (self.drop_guard, self.join_handle)
    }
}

pub(crate) async fn spawn(
    config: Arc<Config>,
    connections: BmcConnectionStore,
    cancel_token: CancellationToken,
) -> Result<Handle, SpawnError> {
    let (cancel_token, drop_guard) = fork_cancel_token(cancel_token);

    let acceptor = Arc::new(ArcSwap::from_pointee(load_tls_acceptor(&config)?));
    let listener = tokio::net::TcpListener::bind(config.api_listen_address).await?;
    let listen_address = listener.local_addr()?;
    let (incoming_tx, incoming_rx) =
        mpsc::channel::<Result<AuthorizedTlsStream, std::io::Error>>(128);
    let accept_task = tokio::spawn({
        let cancel_token = cancel_token.clone();
        let acceptor = acceptor.clone();
        async move {
            while let Some(result) = cancel_token.run_until_cancelled(listener.accept()).await {
                let (tcp, _) = match result {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        // We want to retry indefinitely if we can't accept (like if we're hitting a
                        // file descriptor limit), but we should yield for a bit so that we're not
                        // consuming a whole CPU core indefinitely.
                        tracing::warn!(%error, "could not accept TCP connection");
                        cancel_token
                            .run_until_cancelled(tokio::time::sleep(Duration::from_millis(100)))
                            .await;
                        continue;
                    }
                };
                let tls = acceptor.load_full();
                let tx = incoming_tx.clone();
                tokio::spawn(async move {
                    let result = tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, tls.accept(tcp)).await;
                    let Ok(Ok(inner)) = result else {
                        return;
                    };
                    let identity = inner
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|certs| certs.first())
                        .and_then(|leaf| {
                            carbide_authn::validate_x509_certificate(leaf.as_ref()).ok()
                        })
                        .map(|id| Arc::<str>::from(id.to_string()));
                    let _ = tx
                        .send(Ok(AuthorizedTlsStream {
                            inner,
                            identity: PeerIdentity(identity),
                        }))
                        .await;
                });
            }
        }
    });

    let reload_task = tokio::spawn({
        let cancel_token = cancel_token.clone();
        let config = config.clone();
        async move {
            let mut delay = TLS_REFRESH_INTERVAL;
            while cancel_token
                .run_until_cancelled(tokio::time::sleep(delay))
                .await
                .is_some()
            {
                match load_tls_acceptor(&config) {
                    Ok(next) => {
                        acceptor.store(Arc::new(next));
                        delay = TLS_REFRESH_INTERVAL;
                        tracing::info!("refreshed private console-log API TLS material");
                    }
                    Err(error) => {
                        delay = TLS_REFRESH_RETRY_DELAY;
                        tracing::error!(%error, "could not refresh private console-log API TLS material; retaining last good configuration");
                    }
                }
            }
        }
    });

    let join_handle = tokio::spawn({
        let cancel_token = cancel_token.clone();
        let service = Service {
            connections,
            allowed_spiffe_id: Arc::from(config.api_allowed_client_spiffe_id.as_str()),
            cancel_token: cancel_token.clone(),
        };
        async move {
            if let Err(error) = Server::builder()
                .add_service(ConsoleLogServiceServer::new(service))
                .serve_with_incoming_shutdown(
                    ReceiverStream::new(incoming_rx),
                    cancel_token.cancelled(),
                )
                .await
            {
                tracing::error!(%error, "private console-log API stopped");
            }
            let _ = accept_task.await;
            let _ = reload_task.await;
        }
    });
    Ok(Handle {
        listen_address,
        join_handle,
        drop_guard,
    })
}

fn load_tls_acceptor(config: &Config) -> Result<TlsAcceptor, std::io::Error> {
    let cert_pem = std::fs::read(&config.client_cert_path)?;
    let key_pem = std::fs::read(&config.client_key_path)?;
    let ca_pem = std::fs::read(&config.forge_root_ca_path)?;
    let certs = rustls_pemfile::certs(&mut cert_pem.as_slice()).collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())?.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "TLS key is missing")
    })?;
    let mut roots = RootCertStore::empty();
    let ca_certs = rustls_pemfile::certs(&mut ca_pem.as_slice()).collect::<Result<Vec<_>, _>>()?;
    let (added, _) = roots.add_parsable_certificates(ca_certs);
    if added == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "client CA contains no trust anchors",
        ));
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .allow_unknown_revocation_status()
        .build()
        .map_err(std::io::Error::other)?;
    let mut server = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .map_err(std::io::Error::other)?;
    server.alpn_protocols = vec![b"h2".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(server)))
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("could not read TLS material: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use russh::ChannelMsg;

    use super::*;
    use crate::bmc::message_proxy::ToFrontendMessage;

    #[test]
    fn authorizes_only_the_configured_spiffe_identity() {
        let allowed = "spiffe://nico.local/nico-system/sa/nico-api";
        let mut request = Request::new(());
        request
            .extensions_mut()
            .insert(PeerIdentity(Some(Arc::from(allowed))));
        assert!(request.authorize(allowed).is_ok());

        let error = request
            .authorize("spiffe://nico.local/nico-system/sa/other")
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn rejects_missing_spiffe_identity() {
        let request = Request::new(());
        let error = request
            .authorize("spiffe://nico.local/nico-system/sa/nico-api")
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn validates_and_defaults_tail_lines() {
        assert_eq!(
            StreamConsoleLogsRequest {
                tail_lines: 0,
                ..Default::default()
            }
            .tail_lines()
            .unwrap(),
            1000
        );
        assert_eq!(
            StreamConsoleLogsRequest {
                tail_lines: 1,
                ..Default::default()
            }
            .tail_lines()
            .unwrap(),
            1
        );
        assert_eq!(
            StreamConsoleLogsRequest {
                tail_lines: 1000,
                ..Default::default()
            }
            .tail_lines()
            .unwrap(),
            1000
        );
        assert_eq!(
            StreamConsoleLogsRequest {
                tail_lines: 1001,
                ..Default::default()
            }
            .tail_lines()
            .unwrap_err()
            .code(),
            tonic::Code::InvalidArgument
        );
    }

    #[tokio::test]
    async fn client_queues_are_independent() {
        let dir = temp_dir::TempDir::new().unwrap();
        let config = Arc::new(Config {
            console_logs_path: dir.path().to_path_buf(),
            ..Config::default()
        });
        let machine_id =
            carbide_uuid::machine::MachineId::new(MachineIdSource::Tpm, [8; 32], MachineType::Host);
        let (frontend_tx, frontend_rx) = broadcast::channel(16);
        let cancel_token = CancellationToken::new();
        let (logger, access) = console_logger::spawn(
            machine_id,
            "127.0.0.1:22".parse().unwrap(),
            frontend_rx,
            config,
            cancel_token.clone(),
        );

        let source_a = access.subscribe().unwrap();
        let snapshot_a = access.snapshot().await.unwrap();
        let source_b = access.subscribe().unwrap();
        let snapshot_b = access.snapshot().await.unwrap();
        let (a_tx, a_rx) = mpsc::channel(1);
        let (b_tx, mut b_rx) = mpsc::channel(16);
        let pump_a = tokio::spawn(pump_live(
            source_a,
            a_tx,
            snapshot_a.watermark,
            cancel_token.clone(),
            access.clone(),
        ));
        let pump_b = tokio::spawn(pump_live(
            source_b,
            b_tx,
            snapshot_b.watermark,
            cancel_token.clone(),
            access,
        ));

        assert!(
            frontend_tx
                .send(ToFrontendMessage::Channel(Arc::new(ChannelMsg::Data {
                    data: Bytes::from_static(b"one\ntwo\nthree\n"),
                })))
                .is_ok()
        );
        for expected in [b"one\n".as_slice(), b"two\n", b"three\n"] {
            let item = tokio::time::timeout(Duration::from_secs(2), b_rx.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(item.as_ref(), expected);
        }

        drop(a_rx);
        assert!(
            frontend_tx
                .send(ToFrontendMessage::Channel(Arc::new(ChannelMsg::Data {
                    data: Bytes::from_static(b"four\n"),
                })))
                .is_ok()
        );
        let item = tokio::time::timeout(Duration::from_secs(2), b_rx.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(item.as_ref(), b"four\n");

        cancel_token.cancel();
        pump_a.await.unwrap();
        pump_b.await.unwrap();
        logger.shutdown_and_wait().await;
    }

    #[tokio::test]
    async fn gap_marker_reports_the_aggregate_count() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut pending = 7;
        assert!(try_send_gap(&tx, &mut pending));
        assert_eq!(pending, 0);
        let marker = rx.recv().await.unwrap().unwrap();
        assert_eq!(
            marker.as_ref(),
            b"[console-log-stream: 7 lines omitted for this client]\n"
        );
    }
}
