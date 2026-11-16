/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Exercise artifact authorization and streaming against authoritative SQL state.
//! Minimal machine rows keep these proofs independent of lifecycle fixtures.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri, header};
use axum::response::Response;
use carbide_authn::middleware::Principal;
use carbide_firmware::downloader::FirmwareDownloadConfig;
use carbide_firmware::{FirmwareArtifactCache, FirmwareDownloader};
use carbide_libmlx_model::firmware::operation::{FirmwareOperation, FirmwareOperationState};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
use futures::StreamExt;
use model::machine::ManagedHostState;
use model::machine::nic_firmware::{NicFirmwareOperation, NicFirmwarePhase, NicFirmwarePurpose};
use model::nic_firmware::{NicFirmwareArtifact, NicFirmwareProfileConfig};
use model::test_support::nic_firmware as fixtures;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::{Notify, Semaphore};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::auth::AuthContext;
use crate::handlers::nic_firmware_artifact::router;

#[path = "nic_firmware_artifact_tls.rs"]
mod tls;

const IMAGE: &[u8] = b"frozen firmware image";
const CONFIGURATION: &[u8] = b"frozen device configuration";

fn machine_id(seed: u8, kind: MachineType) -> MachineId {
    MachineId::new(MachineIdSource::ProductBoardChassisSerial, [seed; 32], kind)
}

fn host_id(seed: u8) -> HostMachineId {
    machine_id(seed, MachineType::Host)
        .try_into()
        .expect("host ID")
}

fn principal(host: HostMachineId) -> Principal {
    Principal::SpiffeMachineIdentifier(host.to_string())
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn admitted(origin: &Origin) -> NicFirmwareOperation {
    let mut operation = fixtures::operation();
    operation.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
    operation.targets[0].supervisor_instance_id = Some("frozen-helper".into());
    operation.targets[0].resolution.entry.image = NicFirmwareArtifact {
        url: format!("{}/image", origin.url).parse().expect("image URL"),
        sha256: digest(IMAGE),
    };
    operation.targets[0].resolution.entry.device_config = Some(NicFirmwareArtifact {
        url: format!("{}/configuration", origin.url)
            .parse()
            .expect("configuration URL"),
        sha256: digest(CONFIGURATION),
    });
    operation.validate().expect("valid admitted operation");
    operation
}

async fn create(pool: &PgPool, id: MachineId, state: ManagedHostState) {
    let mut transaction = pool.begin().await.expect("begin machine fixture");
    db::machine::create(&mut transaction, None, &id, state, None, 2)
        .await
        .expect("create minimal machine");
    transaction.commit().await.expect("commit machine fixture");
}

async fn seed(pool: &PgPool, host: HostMachineId, operation: Option<NicFirmwareOperation>) {
    let state = operation
        .map(|operation| ManagedHostState::NicFirmwareUpdating { operation })
        .unwrap_or(ManagedHostState::Ready);
    // Direct state replacement also allows a deliberately invalid persisted
    // fixture; the production route must validate it before cache access.
    sqlx::query("UPDATE machines SET controller_state = $2 WHERE id = $1")
        .bind(host)
        .bind(sqlx::types::Json(state))
        .execute(pool)
        .await
        .expect("replace machine state");
}

struct CacheFixture {
    root: tempfile::TempDir,
    cache: FirmwareArtifactCache,
    cancel: CancellationToken,
    tasks: JoinSet<()>,
}

fn cache(capacity: u64) -> CacheFixture {
    let root = tempfile::tempdir().expect("cache fixture directory");
    let downloader = FirmwareDownloader::with_config(
        FirmwareDownloadConfig {
            max_download_bytes: 64,
            ..Default::default()
        },
        carbide_instrument::Instrumentation::from_meter_provider(
            &opentelemetry::metrics::noop::NoopMeterProvider::new(),
        ),
    )
    .expect("bounded fixture downloader");
    let cancel = CancellationToken::new();
    let mut tasks = JoinSet::new();
    let cache = FirmwareArtifactCache::new(
        root.path().join("artifacts"),
        capacity,
        downloader,
        cancel.clone(),
        &mut tasks,
    )
    .expect("cache fixture");
    CacheFixture {
        root,
        cache,
        cancel,
        tasks,
    }
}

impl CacheFixture {
    fn namespace(&self) -> std::path::PathBuf {
        self.root.path().join("artifacts/digest-v1")
    }

    async fn finish(mut self) {
        self.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(result) = self.tasks.join_next().await {
                result.expect("cache lifecycle task");
            }
        })
        .await
        .expect("cache lifecycle shutdown");
    }
}

fn uri(operation: &NicFirmwareOperation, digest: &str, role: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("operation_id", &operation.operation_id)
        .finish();
    format!("/firmware/nic/{digest}/{role}?{query}")
}

async fn request(
    router: &Router,
    identity: Option<Principal>,
    method: Method,
    uri: &str,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("artifact request");
    if let Some(identity) = identity {
        let mut auth = AuthContext::default();
        auth.principals.push(identity);
        request.extensions_mut().insert(auth);
    }
    router
        .clone()
        .oneshot(request)
        .await
        .expect("router response")
}

async fn settled(router: &Router, host: HostMachineId, uri: &str) -> Response {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = request(router, Some(principal(host)), Method::GET, uri).await;
            if response.status() != StatusCode::ACCEPTED {
                return response;
            }
            // Poll the documented pending response until the cache publishes;
            // the timeout diagnoses a missing completion without guessing a delay.
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("artifact publication completed")
}

async fn ready(router: &Router, host: HostMachineId, uri: &str) -> Response {
    let response = settled(router, host, uri).await;
    assert_eq!(response.status(), StatusCode::OK);
    response
}

#[derive(Clone)]
struct OriginState {
    requests: Arc<Mutex<Vec<String>>>,
    requested: Arc<Notify>,
    responses: Arc<Semaphore>,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

// A local artifact server records source access before pausing its response.
// Tests release response permits explicitly and join the server at completion.
struct Origin {
    url: String,
    state: OriginState,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl Origin {
    async fn start(blocked: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind artifact origin");
        let url = format!("http://{}", listener.local_addr().expect("origin address"));
        let state = OriginState {
            requests: Default::default(),
            requested: Default::default(),
            responses: Arc::new(Semaphore::new(if blocked { 0 } else { 64 })),
            files: Arc::new(Mutex::new(HashMap::from([
                ("/image".into(), IMAGE.to_vec()),
                ("/configuration".into(), CONFIGURATION.to_vec()),
                ("/replacement".into(), b"new catalog contents".to_vec()),
            ]))),
        };
        let app = Router::new()
            .fallback(
                |State(state): State<OriginState>, uri: Uri, headers: HeaderMap| async move {
                    // The origin is a new anonymous request, even when its caller
                    // authenticated to Core and supplied other private headers.
                    assert!(!headers.contains_key(header::AUTHORIZATION));
                    assert!(!headers.contains_key("x-artifact-request-secret"));
                    state
                        .requests
                        .lock()
                        .expect("origin requests")
                        .push(uri.path().to_string());
                    state.requested.notify_one();
                    let permit = state
                        .responses
                        .acquire()
                        .await
                        .expect("origin response gate");
                    permit.forget();
                    state
                        .files
                        .lock()
                        .expect("origin files")
                        .get(uri.path())
                        .expect("known origin path")
                        .clone()
                },
            )
            .with_state(state.clone());
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
                .expect("artifact origin server");
        });
        Self {
            url,
            state,
            stop,
            task,
        }
    }

    fn paths(&self) -> Vec<String> {
        self.state.requests.lock().expect("origin requests").clone()
    }

    async fn finish(self) {
        self.stop.cancel();
        self.task.await.expect("join artifact origin");
    }
}

// Exercise the mounted route against persisted work. Every denial must precede
// cache creation and origin access; this router has no general RBAC layer.
#[crate::sqlx_test]
async fn artifact_rejections_never_contact_the_origin(pool: PgPool) {
    let origin = Origin::start(false).await;
    let host = host_id(1);
    create(&pool, host.into(), ManagedHostState::Ready).await;
    let operation = admitted(&origin);
    let allowed = uri(&operation, &digest(IMAGE), "image");
    let cache = cache(256);
    let app = router(pool.clone(), cache.cache.clone(), cache.cancel.clone());

    struct Case {
        scenario: &'static str,
        identity: Option<Principal>,
        operation: Option<NicFirmwareOperation>,
        uri: String,
        method: Method,
        expected: StatusCode,
    }
    let case = |scenario, expected| Case {
        scenario,
        identity: Some(principal(host)),
        operation: Some(operation.clone()),
        uri: allowed.clone(),
        method: Method::GET,
        expected,
    };
    let mut cases = vec![
        Case {
            identity: None,
            ..case("anonymous", StatusCode::UNAUTHORIZED)
        },
        Case {
            identity: Some(Principal::SpiffeServiceIdentifier(
                "firmware-service".into(),
            )),
            ..case("service identity", StatusCode::UNAUTHORIZED)
        },
        Case {
            identity: Some(Principal::SpiffeMachineIdentifier(
                machine_id(2, MachineType::Dpu).to_string(),
            )),
            ..case("DPU identity", StatusCode::FORBIDDEN)
        },
        Case {
            identity: Some(Principal::SpiffeMachineIdentifier("malformed".into())),
            ..case("malformed identity", StatusCode::FORBIDDEN)
        },
        Case {
            identity: Some(principal(host_id(2))),
            ..case("another host", StatusCode::NOT_FOUND)
        },
        Case {
            operation: None,
            ..case("inactive host", StatusCode::NOT_FOUND)
        },
        Case {
            uri: uri(&operation, &digest(IMAGE), "configuration"),
            ..case("digest belongs to another role", StatusCode::NOT_FOUND)
        },
        Case {
            uri: uri(&operation, &"cd".repeat(32), "image"),
            ..case("unrelated digest", StatusCode::NOT_FOUND)
        },
        Case {
            uri: uri(&operation, &digest(IMAGE), "arbitrary"),
            ..case("unknown role", StatusCode::NOT_FOUND)
        },
        Case {
            uri: uri(&operation, "invalid", "image"),
            ..case("malformed digest", StatusCode::BAD_REQUEST)
        },
        Case {
            uri: format!("/firmware/nic/{}/image", digest(IMAGE)),
            ..case("missing operation token", StatusCode::BAD_REQUEST)
        },
        Case {
            uri: format!("{allowed}&operation_id=duplicate"),
            ..case("ambiguous operation token", StatusCode::BAD_REQUEST)
        },
        Case {
            // Empty separators leave the decoded ID valid, so only the
            // independent encoded-query limit can reject this request.
            uri: format!("{allowed}{}", "&".repeat(1025)),
            ..case("bounded query", StatusCode::BAD_REQUEST)
        },
        Case {
            method: Method::HEAD,
            ..case(
                "HEAD cannot trigger download",
                StatusCode::METHOD_NOT_ALLOWED,
            )
        },
    ];
    cases.push(Case {
        uri: format!(
            "/firmware/nic/{}/image?operation_id={}",
            digest(IMAGE),
            "a".repeat(257)
        ),
        ..case("decoded operation token bound", StatusCode::BAD_REQUEST)
    });
    cases.push(Case {
        uri: format!("{allowed}&source=http://unrelated.invalid/image"),
        ..case(
            "unknown query key cannot select a source",
            StatusCode::BAD_REQUEST,
        )
    });
    for (scenario, encoded_id) in [
        ("empty operation identifier", ""),
        ("non-graphic operation identifier", "%20"),
    ] {
        cases.push(Case {
            uri: format!(
                "/firmware/nic/{}/image?operation_id={encoded_id}",
                digest(IMAGE)
            ),
            ..case(scenario, StatusCode::BAD_REQUEST)
        });
    }
    let mut observing = operation.clone();
    observing.phase = NicFirmwarePhase::ObservingSupervisor { target_index: 0 };
    cases.push(Case {
        operation: Some(observing),
        ..case(
            "initial observation without a receipt",
            StatusCode::NOT_FOUND,
        )
    });
    let mut wrong_id = operation.clone();
    wrong_id.operation_id = "other-workflow".into();
    cases.push(Case {
        operation: Some(wrong_id),
        ..case("stale operation token", StatusCode::NOT_FOUND)
    });
    let mut wrong_phase = operation.clone();
    wrong_phase.phase = NicFirmwarePhase::Unlocking { target_index: 0 };
    cases.push(Case {
        operation: Some(wrong_phase),
        ..case(
            "artifact unavailable before submission",
            StatusCode::NOT_FOUND,
        )
    });
    let mut restoring = operation.clone();
    restoring.phase = NicFirmwarePhase::RestoringLockdown {
        target_index: 0,
        failure: Some("stopped before firmware submission".into()),
    };
    restoring
        .validate()
        .expect("valid stopped workflow restoring lockdown");
    cases.push(Case {
        operation: Some(restoring),
        ..case(
            "lockdown restoration has no artifact authority",
            StatusCode::NOT_FOUND,
        )
    });
    let mut invalid = operation.clone();
    invalid.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 1 };
    cases.push(Case {
        operation: Some(invalid),
        ..case(
            "invalid persisted target index",
            StatusCode::SERVICE_UNAVAILABLE,
        )
    });
    let mut no_configuration = operation.clone();
    no_configuration.targets[0].resolution.entry.device_config = None;
    cases.push(Case {
        operation: Some(no_configuration),
        uri: uri(&operation, &digest(CONFIGURATION), "configuration"),
        ..case("absent optional configuration", StatusCode::NOT_FOUND)
    });
    let mut future_card = operation.clone();
    let mut future_target = fixtures::target();
    future_target.selector =
        model::nic_firmware::NicFirmwareCardSelector::Mac(mac_address::MacAddress::new([
            2, 0, 0, 0, 0, 2,
        ]));
    future_target.pci_name = "0000:02:00.0".into();
    future_target.resolution.entry.image.sha256 = "ef".repeat(32);
    future_card.targets.push(future_target);
    cases.push(Case {
        operation: Some(future_card),
        uri: uri(&operation, &"ef".repeat(32), "image"),
        ..case("another frozen card is not current", StatusCode::NOT_FOUND)
    });
    for (scenario, receipt_state) in [
        ("completed execution", FirmwareOperationState::Complete),
        (
            "uncertain terminal execution",
            FirmwareOperationState::Interrupted,
        ),
    ] {
        let mut settled = operation.clone();
        settled.targets[0].receipt = Some(FirmwareOperation {
            operation_id: operation.operation_id.clone(),
            state: receipt_state,
            phase: None,
            report: Default::default(),
            diagnostic: None,
        });
        cases.push(Case {
            operation: Some(settled),
            ..case(scenario, StatusCode::NOT_FOUND)
        });
    }
    let mut unrelated_receipt = operation.clone();
    unrelated_receipt.targets[0].receipt = Some(FirmwareOperation {
        operation_id: "unrelated-receipt".into(),
        state: FirmwareOperationState::Running,
        phase: None,
        report: Default::default(),
        diagnostic: None,
    });
    cases.push(Case {
        operation: Some(unrelated_receipt),
        ..case("uncorrelated receipt", StatusCode::NOT_FOUND)
    });
    for case in cases {
        seed(&pool, host, case.operation).await;
        let response = request(&app, case.identity, case.method.clone(), &case.uri).await;
        assert_eq!(response.status(), case.expected, "{}", case.scenario);
        assert!(origin.paths().is_empty(), "{}", case.scenario);
        assert!(!cache.namespace().exists(), "{}", case.scenario);
        if case.method == Method::HEAD {
            assert_eq!(
                response.headers()[header::ALLOW],
                "GET",
                "{}",
                case.scenario
            );
        }
    }
    cache.finish().await;
    origin.finish().await;
}

// Edit the catalog while the saved allocation owner is absent. A predicted
// host's live observed receipt must still stage the original bytes for recovery.
#[crate::sqlx_test]
async fn frozen_artifacts_stream_during_observation_after_catalog_changes_without_owner(
    pool: PgPool,
) {
    let origin = Origin::start(true).await;
    let host: HostMachineId = machine_id(3, MachineType::PredictedHost)
        .try_into()
        .expect("predicted host ID");
    let mut operation = admitted(&origin);
    operation.operation_id = "printable/operation?#".into();
    operation.phase = NicFirmwarePhase::ObservingSupervisor { target_index: 0 };
    operation.purpose = NicFirmwarePurpose::Allocation {
        instance_id: InstanceId::new(),
    };
    operation.targets[0].receipt = Some(FirmwareOperation {
        operation_id: operation.operation_id.clone(),
        state: FirmwareOperationState::Running,
        phase: None,
        report: Default::default(),
        diagnostic: None,
    });
    create(
        &pool,
        host.into(),
        ManagedHostState::NicFirmwareUpdating {
            operation: operation.clone(),
        },
    )
    .await;
    let mut transaction = pool.begin().await.expect("catalog transaction");
    let profile = db::nic_firmware::create(
        &mut transaction,
        &operation.targets[0].resolution.profile_id,
        &NicFirmwareProfileConfig {
            entries: vec![operation.targets[0].resolution.entry.clone()],
        },
    )
    .await
    .expect("create admitted catalog revision");
    let mut replacement = profile.config.clone();
    replacement.entries[0].image.url = format!("{}/replacement", origin.url)
        .parse()
        .expect("replacement URL");
    replacement.entries[0].image.sha256 = digest(b"new catalog contents");
    db::nic_firmware::update(&mut transaction, &profile.id, &replacement, profile.version)
        .await
        .expect("replace mutable catalog");
    transaction
        .commit()
        .await
        .expect("commit mutable catalog edit");
    let cache = cache(256);
    let app = router(pool.clone(), cache.cache.clone(), cache.cancel.clone());
    let image_uri = uri(&operation, &digest(IMAGE), "image");
    let pending = request(&app, Some(principal(host)), Method::GET, &image_uri).await;
    assert_eq!(pending.status(), StatusCode::ACCEPTED);
    assert_eq!(pending.headers()[header::RETRY_AFTER], "5");
    tokio::time::timeout(Duration::from_secs(10), origin.state.requested.notified())
        .await
        .expect("origin observed the accepted transfer");
    drop(pending);
    // Complete the accepted transfer after the initiating request has ended.
    origin.state.responses.add_permits(2);
    for (role, bytes) in [("image", IMAGE), ("configuration", CONFIGURATION)] {
        let response = ready(&app, host, &uri(&operation, &digest(bytes), role)).await;
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH]
                .to_str()
                .expect("content length"),
            bytes.len().to_string()
        );
        assert!(!response.headers().contains_key(header::LOCATION));
        assert_eq!(
            to_bytes(response.into_body(), 1024)
                .await
                .expect("stream artifact"),
            bytes
        );
    }
    // Accepted work can fetch its inputs before Running is observed; a
    // cancellation request may still need them to finish its current safe phase.
    for state in [
        FirmwareOperationState::Accepted,
        FirmwareOperationState::CancelRequested,
    ] {
        operation.phase = NicFirmwarePhase::UpdatingFirmware { target_index: 0 };
        operation.targets[0]
            .receipt
            .as_mut()
            .expect("live receipt")
            .state = state;
        seed(&pool, host, Some(operation.clone())).await;
        let response = ready(&app, host, &image_uri).await;
        assert_eq!(
            to_bytes(response.into_body(), 1024)
                .await
                .expect("live receipt bytes"),
            IMAGE
        );
    }
    assert_eq!(origin.paths(), ["/image", "/configuration"]);
    cache.finish().await;
    origin.finish().await;
}

// Header return alone must not free request admission. Only consuming or
// dropping the response body permits another authorized stream.
#[crate::sqlx_test]
async fn response_bodies_hold_admission_until_completion_or_disconnect(pool: PgPool) {
    let origin = Origin::start(false).await;
    let host = host_id(4);
    let operation = admitted(&origin);
    create(
        &pool,
        host.into(),
        ManagedHostState::NicFirmwareUpdating {
            operation: operation.clone(),
        },
    )
    .await;
    let cache = cache(256);
    let app = router(pool, cache.cache.clone(), cache.cancel.clone());
    let uri = uri(&operation, &digest(IMAGE), "image");
    let first = ready(&app, host, &uri).await;
    let mut bodies = vec![first.into_body()];
    for _ in 1..32 {
        let response = request(&app, Some(principal(host)), Method::GET, &uri).await;
        assert_eq!(response.status(), StatusCode::OK);
        bodies.push(response.into_body());
    }
    let full = request(&app, Some(principal(host)), Method::GET, &uri).await;
    assert_eq!(full.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(full.headers()[header::RETRY_AFTER], "5");
    assert_eq!(
        to_bytes(full.into_body(), 1024)
            .await
            .expect("overload body"),
        "firmware service is busy"
    );
    // Identity and syntax rejection stay deterministic even while all stream
    // permits are occupied. Neither denial needs a database or cache lookup.
    for (identity, requested, expected) in [
        (None, uri.as_str(), StatusCode::UNAUTHORIZED),
        (
            Some(principal(host)),
            "/firmware/nic/not-a-digest/image?operation_id=valid",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = request(&app, identity, Method::GET, requested).await;
        assert_eq!(response.status(), expected);
    }
    drop(bodies.pop().expect("disconnect one reader"));
    let replacement = request(&app, Some(principal(host)), Method::GET, &uri).await;
    assert_eq!(replacement.status(), StatusCode::OK);
    bodies.push(replacement.into_body());
    let mut completed = bodies
        .pop()
        .expect("complete another reader")
        .into_data_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = completed.next().await {
        bytes.extend_from_slice(&chunk.expect("stream chunk"));
    }
    assert_eq!(bytes, IMAGE);
    let after_completion = request(&app, Some(principal(host)), Method::GET, &uri).await;
    assert_eq!(after_completion.status(), StatusCode::OK);
    assert_eq!(origin.paths(), ["/image"]);
    drop(after_completion);
    drop(completed);
    drop(bodies);
    cache.finish().await;
    origin.finish().await;
}

// Retention includes both host ID kinds, excludes DPU/inactive rows, and fails
// the request if another host's saved operation or the database cannot be read.
#[crate::sqlx_test]
async fn retention_reads_only_current_host_operations_and_db_failure_denies_access(pool: PgPool) {
    // A validation or decoding denial must return the healthy snapshot session,
    // so pin one connection and compare its actual backend before and after.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .expect("single-connection retention fixture");
    let origin = Origin::start(false).await;
    let mut operation = admitted(&origin);
    for (seed, kind, active) in [
        (5, MachineType::Host, true),
        (6, MachineType::PredictedHost, true),
        (7, MachineType::Dpu, true),
        (8, MachineType::Host, false),
    ] {
        operation.operation_id = format!("operation-{seed}");
        let state = if active {
            ManagedHostState::NicFirmwareUpdating {
                operation: operation.clone(),
            }
        } else {
            ManagedHostState::Ready
        };
        create(&pool, machine_id(seed, kind), state).await;
    }
    let mut operations = db::machine::find_host_nic_firmware_operations(&pool)
        .await
        .expect("focused operation read");
    operations.sort_by(|(_, left), (_, right)| left.operation_id.cmp(&right.operation_id));
    assert_eq!(
        operations
            .iter()
            .map(|(_, operation)| operation.operation_id.as_str())
            .collect::<Vec<_>>(),
        ["operation-5", "operation-6"]
    );
    // The new coherent snapshot must select the same stable/predicted hosts;
    // including a DPU copy would make otherwise valid artifact requests fail.
    let mut session = db::DbSessionGuard::acquire(&pool).await.unwrap();
    session
        .begin_retention_read(Duration::from_secs(5))
        .await
        .unwrap();
    db::nic_firmware_artifact::lock_retention(session.as_pgconn())
        .await
        .unwrap();
    let mut from_snapshot = db::nic_firmware_artifact::snapshot(session.as_pgconn(), host_id(5))
        .await
        .unwrap()
        .unwrap()
        .into_host_operations()
        .unwrap();
    session.commit_retention_read().await.unwrap();
    from_snapshot.sort_by(|(_, left), (_, right)| left.operation_id.cmp(&right.operation_id));
    assert_eq!(
        from_snapshot
            .iter()
            .map(|(id, operation)| (*id, operation.operation_id.as_str()))
            .collect::<Vec<_>>(),
        operations
            .iter()
            .map(|(id, operation)| (*id, operation.operation_id.as_str()))
            .collect::<Vec<_>>(),
    );
    let cache = cache(256);
    let app = router(pool.clone(), cache.cache.clone(), cache.cancel.clone());
    let requested = uri(&operations[0].1, &digest(IMAGE), "image");
    let mut invalid = operations[1].1.clone();
    invalid.phase = NicFirmwarePhase::UpdatingFirmware {
        target_index: usize::MAX,
    };
    seed(&pool, operations[1].0, Some(invalid)).await;
    let backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .expect("retention fixture backend");
    let invalid_retention =
        request(&app, Some(principal(host_id(5))), Method::GET, &requested).await;
    assert_eq!(invalid_retention.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(invalid_retention.headers()[header::RETRY_AFTER], "5");
    assert_eq!(
        to_bytes(invalid_retention.into_body(), 1024).await.unwrap(),
        "firmware state unavailable"
    );
    assert!(!cache.namespace().exists());
    assert!(origin.paths().is_empty());
    let after_denial: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .expect("backend after validation denial");
    assert_eq!(after_denial, backend);
    // The authenticated host is valid, but a broken retention snapshot cannot
    // authorize eviction or a new source transfer on its behalf.
    let nested = format!("{}null{}", "[".repeat(150), "]".repeat(150));
    sqlx::query("UPDATE machines SET controller_state = $2::text::jsonb WHERE id = $1")
        .bind(machine_id(6, MachineType::PredictedHost))
        // Bind text so the fixture itself does not parse through serde_json's
        // depth limit. A denied request must never parse this unrelated value.
        .bind(format!(
            "{{\"state\":\"nicfirmwareupdating\",\"operation\":{nested}}}"
        ))
        .execute(&pool)
        .await
        .expect("corrupt another host's retained operation");
    // Authorization still rejects this operation ID before decoding unrelated
    // retention, even though the same snapshot contains corrupt membership.
    let unknown = requested.replace("operation-5", "unknown-operation");
    let denied = request(&app, Some(principal(host_id(5))), Method::GET, &unknown).await;
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    let unavailable_retention =
        request(&app, Some(principal(host_id(5))), Method::GET, &requested).await;
    assert_eq!(
        unavailable_retention.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(unavailable_retention.headers()[header::RETRY_AFTER], "5");
    assert_eq!(
        to_bytes(unavailable_retention.into_body(), 1024)
            .await
            .unwrap(),
        "firmware state unavailable"
    );
    let after_decode: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .expect("backend after decode denial");
    assert_eq!(after_decode, backend);
    assert!(origin.paths().is_empty());
    // The successful callback must also return this same healthy session after
    // both cold Pending and warm Ready responses, rather than reconnecting.
    seed(&pool, operations[1].0, None).await;
    let successful = ready(&app, host_id(5), &requested).await;
    assert_eq!(to_bytes(successful.into_body(), 1024).await.unwrap(), IMAGE);
    let after_success: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .expect("backend after successful artifact open");
    assert_eq!(after_success, backend);
    pool.close().await;
    let response = request(&app, Some(principal(host_id(5))), Method::GET, &requested).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(origin.paths(), ["/image"]);
    cache.finish().await;
    origin.finish().await;
}

// Retention covers a future card's configuration even during supervisor
// observation with an uncertain receipt. Failed workflows release those files.
#[crate::sqlx_test]
async fn uncertain_receipts_retain_artifacts_but_failed_workflows_release_them(pool: PgPool) {
    let origin = Origin::start(false).await;
    let host = host_id(9);
    let other_host = host_id(10);
    let mut operation = admitted(&origin);
    operation.targets[0].resolution.entry.device_config = None;
    let warmer_host = host_id(14);
    let mut warmer = operation.clone();
    warmer.operation_id = "cache-warmer".into();
    warmer.targets[0].resolution.entry.image = NicFirmwareArtifact {
        url: format!("{}/configuration", origin.url)
            .parse()
            .expect("warm image URL"),
        sha256: digest(CONFIGURATION),
    };
    let mut other = operation.clone();
    other.operation_id = "other-workflow".into();
    let mut future = other.targets[0].clone();
    future.selector =
        model::nic_firmware::NicFirmwareCardSelector::Mac(mac_address::MacAddress::new([
            2, 0, 0, 0, 0, 2,
        ]));
    future.pci_name = "0000:02:00.0".into();
    future.resolution.entry.device_config = Some(warmer.targets[0].resolution.entry.image.clone());
    other.targets.push(future);
    other.phase = NicFirmwarePhase::ObservingSupervisor { target_index: 0 };
    other.targets[0].receipt = Some(FirmwareOperation {
        operation_id: other.operation_id.clone(),
        state: FirmwareOperationState::Interrupted,
        phase: None,
        report: Default::default(),
        diagnostic: None,
    });
    other.validate().expect("uncertain frozen workflow fixture");
    for (id, operation) in [
        (host, operation.clone()),
        (other_host, other.clone()),
        (warmer_host, warmer.clone()),
    ] {
        create(
            &pool,
            id.into(),
            ManagedHostState::NicFirmwareUpdating { operation },
        )
        .await;
    }
    // Warm via separate admitted work, then retire that work. The bytes are now
    // retained only as a future card's configuration in the observing workflow.
    let cache = cache(64);
    let app = router(pool.clone(), cache.cache.clone(), cache.cancel.clone());
    let response = ready(
        &app,
        warmer_host,
        &uri(&warmer, &digest(CONFIGURATION), "image"),
    )
    .await;
    to_bytes(response.into_body(), 1024)
        .await
        .expect("finish retained artifact reader");
    seed(&pool, warmer_host, None).await;
    let requested = uri(&operation, &digest(IMAGE), "image");
    let full = request(&app, Some(principal(host)), Method::GET, &requested).await;
    assert_eq!(full.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(full.headers()[header::RETRY_AFTER], "5");
    assert_eq!(
        to_bytes(full.into_body(), 1024)
            .await
            .expect("capacity response"),
        "firmware cache is full"
    );
    assert_eq!(origin.paths(), ["/configuration"]);

    // A safely settled failure has a different retention contract from an
    // interrupted receipt whose execution effects remain uncertain.
    other.targets[0]
        .receipt
        .as_mut()
        .expect("saved receipt")
        .state = FirmwareOperationState::Failed;
    other.phase = NicFirmwarePhase::Failed {
        reason: "failed before mutation".into(),
    };
    other.validate().expect("safely failed workflow fixture");
    seed(&pool, other_host, Some(other)).await;
    let response = ready(&app, host, &requested).await;
    assert_eq!(
        to_bytes(response.into_body(), 1024)
            .await
            .expect("stream newly admitted image"),
        IMAGE
    );
    assert_eq!(origin.paths(), ["/configuration", "/image"]);
    cache.finish().await;
    origin.finish().await;
}

// A real checksum failure must become a retryable wire error, without publishing
// the bad image. Repairing that same URL must allow the next explicit attempt.
#[crate::sqlx_test]
async fn checksum_failure_can_retry_the_same_repaired_origin(pool: PgPool) {
    let origin = Origin::start(false).await;
    origin
        .state
        .files
        .lock()
        .expect("origin files")
        .insert("/image".into(), b"incorrect image".to_vec());
    let host = host_id(11);
    let operation = admitted(&origin);
    create(
        &pool,
        host.into(),
        ManagedHostState::NicFirmwareUpdating {
            operation: operation.clone(),
        },
    )
    .await;
    let cache = cache(256);
    let app = router(pool, cache.cache.clone(), cache.cancel.clone());
    let requested = uri(&operation, &digest(IMAGE), "image");
    let failed = settled(&app, host, &requested).await;
    assert_eq!(failed.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(failed.headers()[header::RETRY_AFTER], "5");
    assert_eq!(
        to_bytes(failed.into_body(), 1024)
            .await
            .expect("failure body"),
        "firmware artifact unavailable"
    );
    assert!(!cache.namespace().join(digest(IMAGE)).exists());
    origin
        .state
        .files
        .lock()
        .expect("origin files")
        .insert("/image".into(), IMAGE.to_vec());
    let repaired = ready(&app, host, &requested).await;
    assert_eq!(
        to_bytes(repaired.into_body(), 1024)
            .await
            .expect("repaired image"),
        IMAGE
    );
    assert_eq!(origin.paths(), ["/image", "/image"]);
    cache.finish().await;
    origin.finish().await;
}

// A blocked statement must not outlive either the cumulative client deadline
// or server cancellation while retaining local pool capacity. Advance only the
// client clock after PostgreSQL confirms its real lock wait; server timing and
// socket work remain real, and the lock stays held during the replacement read.
#[crate::sqlx_test]
async fn blocked_snapshots_release_pool_capacity_on_deadline_and_cancellation(pool: PgPool) {
    enum Stop {
        Deadline,
        Cancellation,
    }
    let cases = [
        (
            "snapshot deadline",
            12,
            Stop::Deadline,
            "firmware state unavailable",
        ),
        (
            "server cancellation",
            13,
            Stop::Cancellation,
            "firmware service unavailable",
        ),
    ];
    for (scenario, seed, stop, expected_body) in cases {
        let origin = Origin::start(false).await;
        let host = host_id(seed);
        let operation = admitted(&origin);
        create(
            &pool,
            host.into(),
            ManagedHostState::NicFirmwareUpdating {
                operation: operation.clone(),
            },
        )
        .await;
        let reader_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(pool.connect_options().as_ref().clone())
            .await
            .expect("single snapshot connection pool");
        let original_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&reader_pool)
            .await
            .expect("original snapshot backend");
        let mut blocker = pool.begin().await.expect("begin schema lock");
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await
            .expect("schema lock backend");
        sqlx::query("LOCK TABLE machines IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .expect("hold schema lock");
        let cache = cache(256);
        let request_cancel = CancellationToken::new();
        let app = router(
            reader_pool.clone(),
            cache.cache.clone(),
            request_cancel.clone(),
        );
        let requested = uri(&operation, &digest(IMAGE), "image");
        let blocked = {
            let app = app.clone();
            let requested = requested.clone();
            tokio::spawn(async move {
                request(&app, Some(principal(host)), Method::GET, &requested).await
            })
        };
        crate::tests::common::postgres::wait_for_blocked_query(
            &pool,
            blocker_pid,
            "SELECT controller_state, COALESCE",
        )
        .await;
        match stop {
            Stop::Cancellation => request_cancel.cancel(),
            Stop::Deadline => {
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(5) + Duration::from_millis(1)).await;
                tokio::time::resume();
            }
        }
        // The client timer is expired, but this independent watchdog and all
        // subsequent socket/pool progress use real time.
        let response = tokio::time::timeout(Duration::from_secs(2), blocked)
            .await
            .expect("blocked request terminates")
            .expect("join blocked request");
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{scenario}"
        );
        assert_eq!(response.headers()[header::RETRY_AFTER], "5");
        assert_eq!(
            to_bytes(response.into_body(), 1024)
                .await
                .expect("snapshot failure body"),
            expected_body,
            "{scenario}"
        );
        assert!(origin.paths().is_empty());
        assert!(!cache.namespace().exists());

        // Check local capacity before doing network I/O: a slow replacement
        // handshake must not be mistaken for a connection still held by SQLx.
        assert_eq!(
            reader_pool.size(),
            0,
            "{scenario}: abandoned session detached"
        );
        // The original statement is still blocked by DDL. A different backend
        // proves the replacement works before the schema lock is released.
        let replacement_pid: i32 = tokio::time::timeout(
            Duration::from_secs(10),
            sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&reader_pool),
        )
        .await
        .expect("replacement connection established")
        .expect("replacement backend");
        assert_ne!(replacement_pid, original_pid);
        blocker.rollback().await.expect("release schema lock");
        let recovered = router(
            reader_pool.clone(),
            cache.cache.clone(),
            CancellationToken::new(),
        );
        let response = ready(&recovered, host, &requested).await;
        assert_eq!(
            to_bytes(response.into_body(), 1024)
                .await
                .expect("recovered artifact"),
            IMAGE
        );
        assert_eq!(origin.paths(), ["/image"]);
        reader_pool.close().await;
        cache.finish().await;
        origin.finish().await;
    }
}

// Real route ownership must keep SQL exclusion through queued physical work,
// even after cancellation and beyond the configured idle timer. A lost backend
// instead fails the release visibly and must not strand cache or pool capacity.
#[crate::sqlx_test]
async fn physical_open_owns_sql_exclusion_and_commit_failure_is_retryable(pool: PgPool) {
    let options = (*pool.connect_options()).clone();
    tokio::task::spawn_blocking(move || {
        // Isolate the blocking queue so its depth identifies this cache worker,
        // without exposing a production test hook or blocking unrelated tests.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all().max_blocking_threads(1).build().unwrap();
        runtime.block_on(async move {
            let control = sqlx::postgres::PgPoolOptions::new()
                .max_connections(4).connect_with(options.clone()).await.unwrap();
            // Open every control connection before the blocking gate, so a
            // later query cannot need DNS lookup on the occupied blocking thread.
            let mut control_connections = Vec::with_capacity(4);
            for _ in 0..4 {
                control_connections.push(control.acquire().await.unwrap());
            }
            drop(control_connections);
            let origin = Origin::start(false).await;
            for (scenario, cancel_request, seed_number) in [
                ("cancelled caller retains the SQL owner", true, 51),
                ("backend loss returns a retryable HTTP error", false, 52),
            ] {
                let host = host_id(seed_number);
                let operation = admitted(&origin);
                create(&control, host.into(), ManagedHostState::NicFirmwareUpdating {
                    operation: operation.clone(),
                }).await;
                let readers = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(1).connect_with(options.clone()).await.unwrap();
                sqlx::query("SELECT set_config('idle_in_transaction_session_timeout', '100', false)")
                    .execute(&readers).await.unwrap();
                let reader_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(&readers).await.unwrap();
                let cache = cache(64);
                std::fs::create_dir_all(cache.namespace()).unwrap();
                std::fs::write(cache.namespace().join(digest(IMAGE)), IMAGE).unwrap();
                let app = router(readers.clone(), cache.cache.clone(), CancellationToken::new());
                let requested = uri(&operation, &digest(IMAGE), "image");

                let (started_tx, started) = tokio::sync::oneshot::channel();
                let (release, released) = std::sync::mpsc::channel();
                let blocker = tokio::task::spawn_blocking(move || {
                    started_tx.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(15))
                        .expect("release isolated physical-work gate");
                });
                tokio::time::timeout(Duration::from_secs(5), started).await
                    .expect("isolated physical-work gate starts").unwrap();
                let get = tokio::spawn({
                    let app = app.clone();
                    let requested = requested.clone();
                    async move { request(&app, Some(principal(host)), Method::GET, &requested).await }
                });
                tokio::time::timeout(Duration::from_secs(5), async {
                    while tokio::runtime::Handle::current().metrics().blocking_queue_depth() == 0 {
                        tokio::task::yield_now().await;
                    }
                }).await.expect("real cache open queued behind the physical gate");

                let mut writer = control.begin().await.unwrap();
                let writer = tokio::spawn(async move {
                    db::nic_firmware_artifact::lock_retention_for_update(&mut writer).await.unwrap();
                    writer.commit().await.unwrap();
                });
                crate::tests::common::postgres::wait_for_blocked_query(
                    &control, reader_pid, "pg_advisory_xact_lock(",
                ).await;
                let get = if cancel_request {
                    get.abort();
                    assert!(get.await.unwrap_err().is_cancelled());
                    None
                } else { Some(get) };
                // This is the actual PostgreSQL idle-expiry boundary, not a
                // scheduling delay: hold physical work past its 100 ms limit.
                sqlx::query("SELECT pg_sleep(0.3)").execute(&control).await.unwrap();
                let shared: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid = $1 AND locktype = 'advisory' AND mode = 'ShareLock' AND granted)",
                ).bind(reader_pid).fetch_one(&control).await.unwrap();
                assert!(shared, "{scenario}: SQL exclusion still held");
                assert!(!writer.is_finished(), "{scenario}: writer still excluded");
                if !cancel_request {
                    // Terminate only this captured backend in the isolated test
                    // database, after its physical work was definitely queued.
                    let killed: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
                        .bind(reader_pid).fetch_one(&control).await.unwrap();
                    assert!(killed);
                }
                release.send(()).unwrap();
                blocker.await.unwrap();
                tokio::time::timeout(Duration::from_secs(5), writer).await
                    .expect("writer progresses after exclusion ends").unwrap();
                if let Some(get) = get {
                    let response = tokio::time::timeout(Duration::from_secs(5), get).await
                        .expect("commit failure reaches HTTP caller").unwrap();
                    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
                    assert_eq!(response.headers()[header::RETRY_AFTER], "5");
                    assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), "firmware state unavailable");
                }
                assert!(origin.paths().is_empty(), "{scenario}: no source access");
                // A fresh successful request proves capacity is reusable. Keep
                // its body unread while a writer acquires exclusion, proving
                // that streaming holds a cache reader but no SQL transaction.
                let response = tokio::time::timeout(Duration::from_secs(5),
                    request(&app, Some(principal(host)), Method::GET, &requested),
                ).await.expect("fresh request after physical completion");
                assert_eq!(response.status(), StatusCode::OK, "{scenario}");
                let mut streaming_writer = control.begin().await.unwrap();
                tokio::time::timeout(Duration::from_secs(5),
                    db::nic_firmware_artifact::lock_retention_for_update(&mut streaming_writer),
                ).await.expect("streaming response has released SQL exclusion").unwrap();
                streaming_writer.commit().await.unwrap();
                assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), IMAGE);
                assert!(origin.paths().is_empty());
                readers.close().await;
                cache.finish().await;
            }
            origin.finish().await;
            control.close().await;
        });
    }).await.expect("isolated physical-work runtime");
}

// The writer commits after the GET starts waiting for its lock. Authorization
// and retention must both see that commit before the cache can evict anything.
#[crate::sqlx_test]
async fn retention_snapshot_sees_membership_committed_while_waiting_for_the_lock(pool: PgPool) {
    let origin = Origin::start(false).await;
    let requesting_host = host_id(41);
    let retaining_host = host_id(42);
    let operation = admitted(&origin);
    create(&pool, requesting_host.into(), ManagedHostState::Ready).await;
    create(&pool, retaining_host.into(), ManagedHostState::Ready).await;
    let cache = cache(64);
    let retained_bytes = [b'r'; 64];
    let retained_digest = digest(&retained_bytes);
    std::fs::create_dir_all(cache.namespace()).unwrap();
    let retained_path = cache.namespace().join(&retained_digest);
    std::fs::write(&retained_path, retained_bytes).unwrap();
    let app = router(pool.clone(), cache.cache.clone(), CancellationToken::new());

    // Simulate the future producer's final transaction, without enabling it.
    let mut writer = pool.begin().await.expect("begin new operation");
    db::nic_firmware_artifact::lock_retention_for_update(&mut writer)
        .await
        .expect("lock new retention membership");
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    let requested = uri(&operation, &digest(IMAGE), "image");
    let response = tokio::spawn(async move {
        request(
            &app,
            Some(principal(requesting_host)),
            Method::GET,
            &requested,
        )
        .await
    });
    crate::tests::common::postgres::wait_for_blocked_query(
        &pool,
        writer_pid,
        "pg_advisory_xact_lock_shared",
    )
    .await;

    // Authorization must also see the writer's commit. Reading this host
    // before acquiring the lock would incorrectly return 404 for the GET.
    sqlx::query("UPDATE machines SET controller_state = $2 WHERE id = $1")
        .bind(requesting_host)
        .bind(sqlx::types::Json(ManagedHostState::NicFirmwareUpdating {
            operation: operation.clone(),
        }))
        .execute(&mut *writer)
        .await
        .expect("authorize requester in the same writer transaction");
    let mut retained_operation = operation;
    retained_operation.operation_id = "new-retained-operation".into();
    retained_operation.targets[0].resolution.entry.image.sha256 = retained_digest;
    retained_operation
        .validate()
        .expect("valid newly retained operation");
    sqlx::query("UPDATE machines SET controller_state = $2 WHERE id = $1")
        .bind(retaining_host)
        .bind(sqlx::types::Json(ManagedHostState::NicFirmwareUpdating {
            operation: retained_operation,
        }))
        .execute(&mut *writer)
        .await
        .expect("save new membership");
    writer.commit().await.expect("commit new operation");

    let response = tokio::time::timeout(Duration::from_secs(5), response)
        .await
        .expect("GET finishes after writer")
        .expect("join GET");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[header::RETRY_AFTER], "5");
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        "firmware cache is full"
    );
    assert_eq!(std::fs::read(&retained_path).unwrap(), retained_bytes);
    assert!(
        origin.paths().is_empty(),
        "retained capacity denies transfer before source access"
    );

    // Retention lasts only as long as the active operation, rather than pinning
    // the cache permanently after a successful lock/commit cycle.
    seed(&pool, retaining_host, None).await;
    let recovered = router(pool.clone(), cache.cache.clone(), CancellationToken::new());
    let operation = admitted(&origin);
    let response = ready(
        &recovered,
        requesting_host,
        &uri(&operation, &digest(IMAGE), "image"),
    )
    .await;
    assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), IMAGE);
    assert!(!retained_path.exists());
    cache.finish().await;
    origin.finish().await;
}
