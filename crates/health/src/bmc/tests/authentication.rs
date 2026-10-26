// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use super::*;

pub(super) fn expire(client: &BmcClient) {
    client.authentication.lock().unwrap().state = CircuitState::Open {
        until: Instant::now() - Duration::from_secs(1),
        backoff: AUTH_CIRCUIT_BACKOFF,
    };
}

#[tokio::test]
async fn only_protected_success_closes_authentication() {
    let cases = [
        ("protected success", true, Ok("body"), true),
        ("public root", false, Ok("body"), false),
        ("credentials rejected", true, Err(auth_error()), false),
        (
            "inconclusive server error",
            true,
            Err(HealthError::HttpError("HTTP 500".into())),
            false,
        ),
    ];
    for (scenario, protected, outcome, closes) in cases {
        let client = test_client();
        let (op, _) = scripted_op(vec![Err(auth_error()), outcome]);
        client.read_with_auth_retry(protected, op).await.ok();
        assert_eq!(
            client.check_authentication(None).is_ok(),
            closes,
            "{scenario}"
        );
        if !protected {
            let (op, attempts) = scripted_op(vec![]);
            client
                .read_with_auth_retry(false, op)
                .await
                .expect_err("public probes stay bounded");
            assert_eq!(attempts.load(AtomicOrdering::SeqCst), 0);
            let (op, _) = scripted_op(vec![Ok("protected")]);
            assert_eq!(
                client.read_with_auth_retry(true, op).await.unwrap(),
                "protected",
                "discovery may follow a public root to a protected resource"
            );
        }
    }
}

#[test]
fn public_root_paths_cannot_validate_credentials() {
    value_scenarios!(is_protected_path:
        "public root" {
            "/redfish/v1" => false,
            "/redfish/v1/?test=1" => false,
            "https://bmc.example/redfish/v1" => false,
        }
        "protected resource" {
            "/redfish/v1/Managers" => true,
        }
    );
}

#[tokio::test]
async fn concurrent_collectors_admit_one_recovery_probe() {
    let (provider, calls) =
        CountingProvider::new(BmcCredentials::SessionToken { token: "t".into() }, None);
    let client = Arc::new(test_bmc(reqwest(), test_addr(), provider, None, 0, None).unwrap());
    client.ensure_credentials().await.unwrap();
    expire(&client);
    let entered = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    let owner = {
        let client = client.clone();
        let entered = entered.clone();
        let released = released.clone();
        tokio::spawn(async move {
            client
                .read_with_auth_retry(true, || async {
                    entered.notify_one();
                    released.notified().await;
                    Err::<(), _>(auth_error())
                })
                .await
        })
    };
    entered.notified().await;
    for _ in 0..100 {
        let (op, attempts) = scripted_op(vec![]);
        client
            .read_with_auth_retry(true, op)
            .await
            .expect_err("recovery already in flight");
        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 0);
    }
    assert_eq!(
        calls.load(AtomicOrdering::SeqCst),
        2,
        "initial fetch and one refresh"
    );
    released.notify_one();
    assert!(owner.await.unwrap().is_err());
    assert_eq!(client.collector_sweep(), CollectorSweep::Skip);
}

#[tokio::test]
async fn cancelling_a_probe_restores_cooldown_and_allows_later_recovery() {
    let client = test_client();
    expire(&client);
    let entered = AtomicBool::new(false);
    let operation = client.read_with_auth_retry(true, || async {
        entered.store(true, Ordering::Release);
        std::future::pending::<Result<(), HealthError>>().await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), operation)
            .await
            .is_err()
    );
    assert!(
        entered.load(Ordering::Acquire),
        "cancelled during the probe"
    );
    assert_eq!(client.collector_sweep(), CollectorSweep::Skip);
    expire(&client);
    let (op, _) = scripted_op(vec![Ok("recovered")]);
    assert_eq!(
        client.read_with_auth_retry(true, op).await.unwrap(),
        "recovered"
    );
}

#[test]
fn stale_probe_cannot_overwrite_a_newer_authentication_decision() {
    let client = test_client();
    expire(&client);
    let stale = client.authentication_probe(true).unwrap().unwrap();
    expire(&client);
    let current = client.authentication_probe(true).unwrap().unwrap();
    stale.succeeded(true);
    drop(stale);
    assert!(client.check_authentication(None).is_err());
    current.succeeded(true);
    drop(current);
    assert!(client.check_authentication(None).is_ok());
}

#[tokio::test]
async fn rejection_from_an_older_generation_does_not_open_authentication() {
    let client = test_client();
    client.ensure_credentials().await.unwrap();
    let failure = client
        .attempt(
            || async {
                // Inject a response whose credential generation has already been
                // superseded. Production credential replacement also takes the write
                // lock, preventing this race while an operation is active.
                client.credential_generation.fetch_add(1, Ordering::AcqRel);
                Err::<(), _>(auth_error())
            },
            None,
        )
        .await
        .expect_err("the original 401 surfaces");
    assert!(
        failure.probe.is_none(),
        "stale rejection must not start recovery"
    );
    assert!(client.check_authentication(None).is_ok());
}

#[derive(Default)]
struct RotatingProvider {
    repaired: AtomicBool,
    calls: AtomicUsize,
}

impl CredentialProvider for RotatingProvider {
    fn fetch_credentials<'a>(
        &'a self,
        _endpoint: &'a BmcAddr,
    ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        let token = if self.repaired.load(Ordering::Acquire) {
            "good"
        } else {
            "bad"
        };
        Box::pin(async move {
            Ok(BmcCredentials::SessionToken {
                token: token.into(),
            })
        })
    }
}

/// Optional BMC_AUTH_TEST_URL runs this same client test against a Postman
/// simulation. The default fixture keeps CI independent of that executable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sensor_and_sse_fanout_is_bounded_over_http_and_recovers() {
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use futures::StreamExt;

    for concurrency in [1, 4] {
        let requests = Arc::new(AtomicUsize::new(0));
        let initial = Arc::new(tokio::sync::Barrier::new(concurrency));
        let counter = requests.clone();
        let app = Router::new()
            .route(
                "/redfish/stats",
                get(move || {
                    let counter = counter.clone();
                    async move { Json(counter.load(AtomicOrdering::SeqCst)) }
                }),
            )
            .route("/redfish/control", post(|| async {}))
            .route(
                "/{*path}",
                get(move |uri: axum::http::Uri, headers: HeaderMap| {
                    let counter = requests.clone();
                    let initial = initial.clone();
                    async move {
                        if uri.path() == "/redfish/healthy" || uri.path() == "/redfish/v1" {
                            return (axum::http::StatusCode::OK, ENTITY_BODY);
                        }
                        let attempt = counter.fetch_add(1, AtomicOrdering::SeqCst);
                        if attempt < concurrency {
                            initial.wait().await;
                        }
                        let status = if headers
                            .get("x-auth-token")
                            .is_some_and(|token| token == "good")
                        {
                            axum::http::StatusCode::OK
                        } else {
                            axum::http::StatusCode::UNAUTHORIZED
                        };
                        (status, ENTITY_BODY)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base_url =
            Url::parse(&std::env::var("BMC_AUTH_TEST_URL").unwrap_or(local_url)).unwrap();
        let http = reqwest::Client::new();
        http.post(base_url.join("/redfish/control").unwrap())
            .query(&[("concurrency", concurrency)])
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let provider = Arc::new(RotatingProvider::default());
        let client = Arc::new(
            BmcClient::new(
                reqwest(),
                test_addr(),
                provider.clone(),
                Some(base_url.clone()),
                0,
                NonZeroUsize::new(concurrency).unwrap(),
                None,
            )
            .unwrap(),
        );
        let sensor = ODataId::from("/redfish/v1/Chassis/1/Sensors/1".to_string());
        let mut pending = futures::stream::FuturesUnordered::new();
        for _ in 0..300 {
            let client = client.clone();
            let sensor = sensor.clone();
            pending.push(tokio::spawn(async move {
                assert!(client.get::<TestEntity>(&sensor).await.is_err());
            }));
        }
        for _ in 0..5 {
            let client = client.clone();
            pending.push(tokio::spawn(async move {
                assert!(
                    client
                        .stream_events::<serde_json::Value>("/redfish/events", None)
                        .await
                        .is_err()
                );
            }));
        }
        while let Some(result) = pending.next().await {
            result.unwrap();
        }
        let stats = || async {
            http.get(base_url.join("/redfish/stats").unwrap())
                .send()
                .await
                .unwrap()
                .json::<usize>()
                .await
                .unwrap()
        };
        assert_eq!(
            stats().await,
            concurrency + 1,
            "bounded initial failures and one retry"
        );
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 2);
        assert!(
            client
                .get::<TestEntity>(&ODataId::service_root())
                .await
                .is_err()
        );
        assert!(
            client
                .get_repaired::<TestEntity>(&sensor, &nv_redfish::core::NoRepair)
                .await
                .is_err()
        );
        assert!(client.delete::<TestEntity>(&sensor).await.is_err());
        assert_eq!(stats().await, concurrency + 1, "all paths share admission");
        let (healthy, _) = client_against(base_url.clone());
        healthy
            .get::<TestEntity>(&ODataId::from("/redfish/healthy".to_string()))
            .await
            .unwrap();

        expire(&client);
        assert!(client.get::<TestEntity>(&sensor).await.is_err());
        assert_eq!(
            stats().await,
            concurrency + 2,
            "one probe without a second retry"
        );
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 3);
        assert!(client.get::<TestEntity>(&sensor).await.is_err());
        assert_eq!(
            stats().await,
            concurrency + 2,
            "another 401 restarts cooldown"
        );

        provider.repaired.store(true, Ordering::Release);
        expire(&client);
        client
            .get_repaired::<TestEntity>(&sensor, &nv_redfish::core::NoRepair)
            .await
            .unwrap();
        client.get::<TestEntity>(&sensor).await.unwrap();
        assert_eq!(
            stats().await,
            concurrency + 4,
            "fresh credentials restore collection"
        );
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 4);
        server.abort();
    }
}
