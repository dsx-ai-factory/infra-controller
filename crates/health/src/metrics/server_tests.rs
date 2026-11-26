// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::io::{Read, Write};
use std::sync::{Weak, mpsc};
use std::time::{Duration, Instant};

use carbide_instrument::testing::{capture_logs, capture_logs_async};
use carbide_test_support::value_scenarios;
use flate2::read::GzDecoder;
use futures::FutureExt;
use futures::future::BoxFuture;
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::*;

#[test]
fn encoding_negotiation_honors_preferences_exclusions_and_malformed_entries() {
    use Encoding::{Gzip, Identity};

    value_scenarios!(negotiate_lines:
        "preferences and exclusions" {
            (&[][..], true) => Some(Identity),
            (&[""][..], true) => Some(Identity),
            (&["  ,  "][..], true) => Some(Identity),
            (&["gzip"][..], false) => Some(Identity),
            (&["GZip"][..], true) => Some(Gzip),
            (&["br"][..], true) => Some(Identity),
            (&["gzip;q=0.4, identity;q=0.8"][..], true) => Some(Identity),
            (&["gzip;q=0.4, identity;q=0.4"][..], true) => Some(Gzip),
            (&["gzip;q=0.001"][..], true) => Some(Gzip),
            (&["*;q=1, gzip;q=0"][..], true) => Some(Identity),
            (&["*;q=0.5"][..], true) => Some(Gzip),
            (&["*;q=0"][..], true) => None,
            (&["*;q=0, identity;q=1"][..], true) => Some(Identity),
            (&["*;q=0, gzip;q=1"][..], true) => Some(Gzip),
            (&["gzip, identity;q=0"][..], false) => None,
            (&["gzip;q=0, identity;q=0"][..], true) => None,
        }

        "combined and malformed entries" {
            (&["identity;q=0.1", "gzip;q=0.5"][..], true) => Some(Gzip),
            (&["gzip;q=0.2, identity;q=0.6", "gzip;q=0.8"][..], true) => Some(Gzip),
            (&["gzip;q=0", "gzip;q=1"][..], true) => Some(Identity),
            (&["gzip;q=1", "gzip;q=0"][..], true) => Some(Identity),
            (&["*;q=0.5, *;q=0"][..], true) => None,
            (&["identity;q=1, identity;q=0"][..], true) => None,
            (&["gzip;q=invalid"][..], true) => Some(Identity),
            (&["gzip;q=1.001"][..], true) => Some(Identity),
            (&["gzip;q=0.1234"][..], true) => Some(Identity),
            (&["gzip;q=.5"][..], true) => Some(Identity),
            (&["gzip;level=1"][..], true) => Some(Identity),
            (&["gzip;q=1;q=0"][..], true) => Some(Identity),
            (&["g zip;q=1, identity;q=0"][..], true) => None,
            (&["gzip;q=0.00x, gzip;q=1"][..], true) => Some(Gzip),
            (&["gzip;q=1., identity;q=0."][..], true) => Some(Gzip),
        }
    );
}

#[test]
fn malformed_bytes_do_not_hide_valid_exclusions_in_the_same_header() {
    let mut headers = HeaderMap::new();

    headers.insert(
        ACCEPT_ENCODING,
        HeaderValue::from_bytes(b"identity;q=0, gzip;q=0, \xff").unwrap(),
    );

    assert_eq!(negotiate(&headers, true), None);
}

fn negotiate_lines((lines, enabled): (&[&str], bool)) -> Option<Encoding> {
    let mut headers = HeaderMap::new();

    for line in lines {
        headers.append(ACCEPT_ENCODING, HeaderValue::from_str(line).unwrap());
    }

    negotiate(&headers, enabled)
}

#[tokio::test]
async fn metrics_routes_preserve_get_methods_status_headers_and_bodies() {
    let manager = Arc::new(MetricsManager::new("test").unwrap());
    let metrics = manager.export_metrics().unwrap();
    let telemetry = manager.export_telemetry().unwrap();

    let plain = "text/plain; charset=utf-8";

    let disallowed = (
        StatusCode::METHOD_NOT_ALLOWED,
        Some("GET"),
        plain,
        "method not allowed; use GET",
    );

    let missing = (
        StatusCode::NOT_FOUND,
        None,
        plain,
        "not found; use /metrics, /telemetry, or /livez",
    );

    let cases = [
        (Method::GET, "/livez", (StatusCode::OK, None, plain, "ok")),
        (
            Method::GET,
            "/metrics",
            (
                StatusCode::OK,
                None,
                prometheus::TEXT_FORMAT,
                metrics.as_str(),
            ),
        ),
        (
            Method::GET,
            "/telemetry",
            (
                StatusCode::OK,
                None,
                prometheus::TEXT_FORMAT,
                telemetry.as_str(),
            ),
        ),
        (Method::POST, "/livez", disallowed),
        (Method::POST, "/metrics", disallowed),
        (Method::HEAD, "/telemetry", disallowed),
        (Method::GET, "/unknown", missing),
        (Method::POST, "/unknown", missing),
    ];

    for (method, path, (status, allow, content_type, body)) in cases {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(())
            .unwrap();

        let response = serve_request(
            request,
            manager.clone(),
            PrometheusSinkConfig::default(),
            export_limit(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), status, "{path}");

        assert_eq!(
            response.headers().get(ALLOW).map(|v| v.to_str().unwrap()),
            allow,
            "{path}"
        );

        assert_eq!(response.headers()[CONTENT_TYPE], content_type, "{path}");

        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            body,
            "{path}"
        );
    }
}

#[tokio::test]
async fn telemetry_http_bodies_preserve_compact_and_generic_metrics_with_gzip() {
    let (manager, registry, _gauge) = recorded_gauge();

    for generic in [false, true] {
        if generic {
            let custom =
                prometheus::Gauge::new("custom_temperature", "Custom temperature").unwrap();

            custom.set(99.0);
            registry.registry().register(Box::new(custom)).unwrap();
        }

        let expected = manager.export_telemetry().unwrap();

        for (enabled, accept, encoding) in [
            (false, "gzip", None),
            (true, "", None),
            (true, "gzip", Some("gzip")),
        ] {
            let config = PrometheusSinkConfig {
                gzip: enabled,
                gzip_level: 1,
            };

            let (mut client, server, shutdown) = connect(manager.clone(), config).await;
            let _shutdown_guard = shutdown.drop_guard();

            let request = format!(
                "GET /telemetry HTTP/1.1\r\nHost: localhost\r\nAccept-Encoding: {accept}\r\nConnection: close\r\n\r\n"
            );

            client.write_all(request.as_bytes()).await.unwrap();
            let mut wire = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut wire))
                .await
                .unwrap()
                .unwrap();

            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();

            let split = wire
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();

            let headers = String::from_utf8(wire[..split].to_vec())
                .unwrap()
                .to_ascii_lowercase();

            let body = &wire[split + 4..];

            assert!(headers.starts_with("http/1.1 200 ok\r\n"), "{headers}");

            assert!(
                headers.contains(&format!("content-length: {}", body.len())),
                "{headers}"
            );

            assert!(
                headers.contains("content-type: text/plain; version=0.0.4"),
                "{headers}"
            );

            assert_eq!(
                headers.contains("vary: accept-encoding"),
                enabled,
                "{headers}"
            );

            assert_eq!(
                headers.contains("content-encoding: gzip"),
                encoding.is_some(),
                "{headers}"
            );

            let actual = if encoding.is_some() {
                let mut decoded = String::new();
                GzDecoder::new(body).read_to_string(&mut decoded).unwrap();
                decoded
            } else {
                String::from_utf8(body.to_vec()).unwrap()
            };

            assert_eq!(actual, expected, "generic={generic}, accept={accept}");
        }
    }
}

#[tokio::test]
async fn unacceptable_telemetry_does_not_start_encoding() {
    for gzip in [false, true] {
        let manager = Arc::new(MetricsManager::new("test").unwrap());

        telemetry::lock(&manager.telemetry_state).before_encode = Some(Arc::new(|_| {
            panic!("an unacceptable representation must not capture an encoding job");
        }));

        let request = Request::builder()
            .uri("/telemetry")
            .header(ACCEPT_ENCODING, "identity;q=0, gzip;q=0")
            .body(())
            .unwrap();

        let response = serve_request(
            request,
            manager,
            PrometheusSinkConfig {
                gzip,
                gzip_level: 1,
            },
            export_limit(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
        assert!(!response.headers().contains_key(CONTENT_ENCODING));

        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "no acceptable telemetry representation"
        );
    }
}

#[tokio::test]
async fn disconnect_and_shutdown_cancel_real_export_jobs_and_release_snapshots() {
    for disconnect in [true, false] {
        let (manager, _registry, gauge) = recorded_gauge();

        let (reading, mut started_rx) = pause_export(&manager, &gauge);

        let (mut client, server, shutdown) =
            connect(manager.clone(), PrometheusSinkConfig::default()).await;

        let _shutdown_guard = shutdown.clone().drop_guard();
        client
            .write_all(b"GET /telemetry HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        let worker = tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
            .await
            .unwrap()
            .unwrap();

        gauge.gauges.clear();

        assert!(!worker.is_cancelled());

        assert!(
            reading.upgrade().is_some(),
            "paused export owns the removed reading"
        );

        let client = if disconnect {
            drop(client);
            None
        } else {
            shutdown.cancel();
            Some(client)
        };

        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            while reading.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        drop(client);

        assert!(worker.is_cancelled(), "disconnect={disconnect}");

        assert!(
            reading.upgrade().is_none(),
            "cancelled job releases its snapshot"
        );
    }
}

#[tokio::test]
async fn aborting_listener_cancels_exports_with_clients_still_connected() {
    let (manager, _registry, gauge) = recorded_gauge();

    let (reading, mut started_rx) = pause_export(&manager, &gauge);

    let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = reservation.local_addr().unwrap();

    drop(reservation);

    let listener = tokio::spawn(run_metrics_server_with_config(
        addr,
        manager.clone(),
        PrometheusSinkConfig::default(),
        CancellationToken::new(),
    ));

    let mut client = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match TcpStream::connect(addr).await {
                Ok(client) => break client,
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("loopback connection failed: {error}"),
            }
        }
    })
    .await
    .unwrap();

    client
        .write_all(b"GET /telemetry HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();

    let worker = tokio::time::timeout(Duration::from_secs(5), started_rx.recv())
        .await
        .unwrap()
        .unwrap();

    let _worker_guard = worker.clone().drop_guard();
    gauge.gauges.clear();

    assert!(!worker.is_cancelled());

    assert!(
        reading.upgrade().is_some(),
        "paused export owns the removed reading"
    );

    listener.abort();

    assert!(listener.await.unwrap_err().is_cancelled());

    tokio::time::timeout(Duration::from_secs(5), async {
        worker.cancelled().await;

        while reading.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    assert!(
        reading.upgrade().is_none(),
        "listener abandonment releases its snapshot"
    );

    drop(client);
}

#[tokio::test]
async fn livez_answers_while_the_callers_runtime_cannot_run_tasks() {
    let (addr, _shutdown, _stopped) = spawn_server(Arc::new(MetricsManager::new("test").unwrap()));

    // The blocking client never yields, so this single-threaded runtime polls
    // nothing: only the server's own thread can answer.
    let response = read_response(send_request(addr, "/livez"));

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.ends_with("\r\n\r\nok"), "{response}");
}

#[test]
fn livez_and_telemetry_answer_and_another_scrape_is_rejected_while_metrics_exports_are_blocked() {
    let (manager, entered, release) = blocked_exports();
    let (addr, _shutdown, _stopped) = spawn_server(manager);
    let mut scrapes = Vec::new();

    // Each scrape has its own connection, as a scrape retry does.
    for _ in 0..METRICS_EXPORT_LIMIT {
        scrapes.push(send_request(addr, "/metrics"));
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    for (path, status) in [
        ("/livez", "HTTP/1.1 200 OK\r\n"),
        ("/telemetry", "HTTP/1.1 200 OK\r\n"),
        ("/metrics", "HTTP/1.1 503 Service Unavailable\r\n"),
    ] {
        let response = read_response(send_request(addr, path));

        assert!(response.starts_with(status), "{path}: {response}");
    }

    drop(release);
}

#[tokio::test]
async fn metrics_export_holds_its_permit_until_it_finishes_after_its_request_is_dropped() {
    let (manager, entered, release) = blocked_exports();
    let exports = export_limit();

    let scrape = || {
        serve_request(
            Request::builder().uri("/metrics").body(()).unwrap(),
            manager.clone(),
            PrometheusSinkConfig::default(),
            exports.clone(),
            CancellationToken::new(),
        )
    };

    // The first poll starts the export. The request is then dropped, as it is
    // when its client disconnects.
    for _ in 0..METRICS_EXPORT_LIMIT {
        assert!(scrape().now_or_never().is_none());
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    let (rejected, logs) = capture_logs_async(scrape())
        .now_or_never()
        .expect("a scrape over the limit is answered without waiting");

    let rejected = rejected.unwrap();

    assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);

    assert_eq!(
        rejected.into_body().collect().await.unwrap().to_bytes(),
        "too many metrics exports in progress"
    );

    let warning = logs
        .iter()
        .find(|log| log.message == "Metrics export rejected; export limit reached")
        .expect("the rejection is logged");

    assert_eq!(warning.level, tracing::Level::WARN);

    drop(release);

    let admitted = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = scrape().await.unwrap();

            if response.status() != StatusCode::SERVICE_UNAVAILABLE {
                break response;
            }

            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the finished exports return their permits");

    assert_eq!(admitted.status(), StatusCode::OK);
}

#[test]
fn export_rejections_are_logged_at_the_start_of_each_run_and_then_once_per_interval() {
    const MESSAGE: &str = "Metrics export rejected; export limit reached";

    let exports = ExportLimit::new();

    let logs = capture_logs(|| {
        // The permit granted at the start of the second pass ends the first run.
        for _ in 0..2 {
            let permits: Vec<_> = (0..METRICS_EXPORT_LIMIT)
                .map(|_| exports.try_acquire().expect("a permit is free"))
                .collect();

            for _ in 0..=EXPORT_REJECTIONS_PER_WARNING {
                assert!(exports.try_acquire().is_none());
            }

            drop(permits);
        }
    });

    let counts: Vec<_> = logs
        .iter()
        .filter(|log| log.level == tracing::Level::WARN && log.message == MESSAGE)
        .map(|log| log.field("consecutive_rejection_count").unwrap())
        .collect();

    let first = "1";
    let next = (EXPORT_REJECTIONS_PER_WARNING + 1).to_string();

    assert_eq!(counts, [first, &next, first, &next]);
}

#[tokio::test]
async fn server_stop_is_reported_while_a_metrics_export_is_blocked() {
    let (manager, entered, release) = blocked_exports();
    let (addr, shutdown, stopped) = spawn_server(manager);
    let _scrape = send_request(addr, "/metrics");

    entered.recv_timeout(Duration::from_secs(5)).unwrap();

    drop(shutdown);

    // The export stays blocked until `release` is dropped, so a server thread
    // that waited for it to finish before reporting would time out here.
    let stopped = tokio::time::timeout(Duration::from_secs(5), stopped)
        .await
        .expect("the server thread reports that it stopped");

    assert!(stopped.is_ok(), "{stopped:?}");

    drop(release);
}

#[tokio::test]
async fn accept_error_is_logged_and_the_next_connection_is_served() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = &listener;
    let shutdown = CancellationToken::new();
    let mut injected = Some(std::io::Error::other("too many open files"));

    let server = serve_connections(
        || {
            let injected = injected.take();

            async move {
                match injected {
                    Some(error) => Err(error),
                    None => listener.accept().await,
                }
            }
        },
        Arc::new(MetricsManager::new("test").unwrap()),
        PrometheusSinkConfig::default(),
        &shutdown,
    );

    let client = async {
        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut response = String::new();

        client
            .write_all(b"GET /livez HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();

        client.read_to_string(&mut response).await.unwrap();
        shutdown.cancel();

        response
    };

    let (served, logs) = capture_logs_async(tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, client)
    }))
    .await;

    let (stopped, response) = served.expect("the server keeps accepting after the error");

    assert!(stopped.is_ok(), "shutdown ends the server without an error");
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");

    let retry = logs
        .iter()
        .find(|log| log.message == "Metrics server accept failed; retrying")
        .expect("the accept error is logged");

    assert_eq!(retry.level, tracing::Level::WARN);
    assert_eq!(retry.field("error"), Some("too many open files"));
}

#[tokio::test(start_paused = true)]
async fn accept_failures_stop_the_server_only_after_an_unbroken_run_of_retries() {
    const MESSAGE: &str = "Metrics server accept failed; retrying";

    let warnings_per_run = ACCEPT_RETRY_LIMIT.div_ceil(ACCEPT_RETRIES_PER_WARNING) as usize;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
    let _client = client.unwrap();

    // Each case accepts the scripted results in order and then fails forever.
    for (scenario, script, retries, warnings) in [
        (
            "every accept fails",
            Vec::new(),
            ACCEPT_RETRY_LIMIT,
            warnings_per_run,
        ),
        (
            "an accepted connection starts a new run",
            vec![Err(std::io::Error::other("connection aborted")), accepted],
            ACCEPT_RETRY_LIMIT + 1,
            warnings_per_run + 1,
        ),
    ] {
        let mut script = script.into_iter();
        let shutdown = CancellationToken::new();
        let start = tokio::time::Instant::now();

        // The timeout turns a server that never stops into a failure.
        let (stopped, logs) = capture_logs_async(tokio::time::timeout(
            ACCEPT_RETRY_DELAY * ACCEPT_RETRY_LIMIT * 2,
            serve_connections(
                || {
                    std::future::ready(
                        script
                            .next()
                            .unwrap_or_else(|| Err(std::io::Error::other("too many open files"))),
                    )
                },
                Arc::new(MetricsManager::new("test").unwrap()),
                PrometheusSinkConfig::default(),
                &shutdown,
            ),
        ))
        .await;

        let error = stopped.expect(scenario).expect_err(scenario);

        assert_eq!(
            error.source.to_string(),
            "too many open files",
            "{scenario}"
        );
        assert_eq!(start.elapsed(), ACCEPT_RETRY_DELAY * retries, "{scenario}");

        assert_eq!(
            logs.iter()
                .filter(|log| log.level == tracing::Level::WARN && log.message == MESSAGE)
                .count(),
            warnings,
            "{scenario}"
        );
    }
}

#[tokio::test]
async fn bind_failure_resolves_the_server_future_with_the_error() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();

    let stopped = spawn_metrics_server(
        occupied.local_addr().unwrap(),
        Arc::new(MetricsManager::new("test").unwrap()),
        PrometheusSinkConfig::default(),
        CancellationToken::new(),
    );

    let error = tokio::time::timeout(Duration::from_secs(5), stopped)
        .await
        .expect("the server thread reports that it stopped")
        .expect_err("the port is occupied");

    assert!(is_addr_in_use(&error), "{error}");
}

/// Starts the server on its own thread at a free loopback port and waits
/// until the port accepts connections. Dropping the guard stops it, and the
/// future then resolves with its result.
fn spawn_server(
    manager: Arc<MetricsManager>,
) -> (
    SocketAddr,
    tokio_util::sync::DropGuard,
    BoxFuture<'static, Result<(), BoxedErr>>,
) {
    let deadline = Instant::now() + Duration::from_secs(5);

    'port: loop {
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reservation.local_addr().unwrap();

        drop(reservation);

        let shutdown = CancellationToken::new();

        let mut stopped = spawn_metrics_server(
            addr,
            manager.clone(),
            PrometheusSinkConfig::default(),
            shutdown.clone(),
        )
        .boxed();

        let shutdown = shutdown.drop_guard();

        loop {
            // Another socket can take the port before the server binds it.
            // The pause lets the server report that first, so a connection
            // to the other socket is not taken for readiness.
            std::thread::sleep(Duration::from_millis(10));

            match stopped.as_mut().now_or_never() {
                None => {}
                Some(Err(error)) if is_addr_in_use(&error) => continue 'port,
                Some(stopped) => panic!("metrics server stopped before serving: {stopped:?}"),
            }

            if std::net::TcpStream::connect(addr).is_ok() {
                return (addr, shutdown, stopped);
            }

            assert!(Instant::now() < deadline, "metrics server did not start");
        }
    }
}

fn export_limit() -> Arc<ExportLimit> {
    Arc::new(ExportLimit::new())
}

/// Collector that blocks a `/metrics` export until the sender of `release` is
/// dropped.
struct BlockedCollector {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl Collector for BlockedCollector {
    fn desc(&self) -> Vec<&Desc> {
        Vec::new()
    }

    fn collect(&self) -> Vec<proto::MetricFamily> {
        self.entered.send(()).unwrap();
        self.release.lock().unwrap().recv().ok();
        Vec::new()
    }
}

/// Returns a manager whose `/metrics` exports block, a receiver that reports
/// each export as it blocks, and the sender whose drop releases them all.
fn blocked_exports() -> (Arc<MetricsManager>, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered, entered_rx) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let manager = Arc::new(MetricsManager::new("test").unwrap());

    manager
        .global_registry()
        .register(Box::new(BlockedCollector {
            entered,
            release: Mutex::new(release),
        }))
        .unwrap();

    (manager, entered_rx, release_tx)
}

fn is_addr_in_use(error: &BoxedErr) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::AddrInUse)
}

/// Sends a request from a blocking client, which needs no runtime to make
/// progress.
fn send_request(addr: SocketAddr, path: &str) -> std::net::TcpStream {
    let mut client = std::net::TcpStream::connect(addr).unwrap();

    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    write!(
        client,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();

    client
}

fn read_response(mut client: std::net::TcpStream) -> String {
    let mut response = String::new();

    client.read_to_string(&mut response).unwrap();

    response
}

async fn connect(
    manager: Arc<MetricsManager>,
    config: PrometheusSinkConfig,
) -> (TcpStream, tokio::task::JoinHandle<()>, CancellationToken) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    let connection = shutdown.child_token();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve_connection(stream, manager, config, export_limit(), connection).await;
    });

    let client = TcpStream::connect(addr).await.unwrap();

    (client, server, shutdown)
}

fn recorded_gauge() -> (Arc<MetricsManager>, CollectorRegistry, Arc<GaugeMetrics>) {
    let manager = Arc::new(MetricsManager::new("test").unwrap());

    let registry = manager
        .create_telemetry_collector_registry("sensor".into(), "test")
        .unwrap();

    let gauge = registry
        .create_gauge_metrics("sensor".into(), "Sensor readings", vec![])
        .unwrap();

    gauge.record(GaugeReading::new(
        "temperature",
        "inlet",
        "temperature",
        "celsius",
        22.5,
    ));

    (manager, registry, gauge)
}

fn pause_export(
    manager: &MetricsManager,
    gauge: &GaugeMetrics,
) -> (Weak<GaugeData>, UnboundedReceiver<CancellationToken>) {
    let reading = Arc::downgrade(
        gauge
            .gauges
            .get(&GaugeKey::from("temperature"))
            .unwrap()
            .value(),
    );

    let (started_tx, started_rx) = unbounded_channel();

    telemetry::lock(&manager.telemetry_state).before_encode = Some(Arc::new(move |cancellation| {
        started_tx.send(cancellation.clone()).unwrap();
        tokio::runtime::Handle::current().block_on(cancellation.cancelled());
    }));

    (reading, started_rx)
}
