// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::io::Read;
use std::sync::Weak;
use std::time::Duration;

use carbide_test_support::value_scenarios;
use flate2::read::GzDecoder;
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
async fn aborting_public_listener_cancels_exports_with_clients_still_connected() {
    let (manager, _registry, gauge) = recorded_gauge();

    let (reading, mut started_rx) = pause_export(&manager, &gauge);

    let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = reservation.local_addr().unwrap();

    drop(reservation);

    let listener = tokio::spawn(run_metrics_server(addr, manager.clone()));

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
        serve_connection(stream, manager, config, connection).await;
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
