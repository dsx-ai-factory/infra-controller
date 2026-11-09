// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

use carbide_test_support::Outcome::{Fails, Yields};
use carbide_test_support::{Case, Check, check_cases_async, check_values};
use futures::FutureExt as _;
use http::{Request, Response};
use k8s_openapi::ByteString;
use kube::client::Body;
use serde_json::{Value, json};
use tower_test::mock;

use super::*;
type ApiHandle = mock::Handle<Request<Body>, Response<Body>>;
const NS: &str = "dpf-operator-system";
const CLUSTER_PATH: &str = "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/dpf-operator-system/dpuclusters/carbide-dpf-cluster";
const REQUEST_PATH: &str = "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/dpf-operator-system/dpuservicecredentialrequests/nico-service-vpc-observer-19205ee90d2f8fe2";
const SECRET_PATH: &str =
    "/api/v1/namespaces/dpf-operator-system/secrets/nico-service-vpc-observer-19205ee90d2f8fe2";
const NAME: &str = "nico-service-vpc-observer-19205ee90d2f8fe2";

/// Supplies the current management DPUCluster identity used to reject stale credential issuance.
fn cluster_identity() -> DpuClusterIdentity {
    DpuClusterIdentity {
        namespace: NS.to_owned(),
        name: "carbide-dpf-cluster".to_owned(),
        uid: "current-cluster-uid".to_owned(),
    }
}

/// Supplies a current DPUCluster whose administrator Secret must never be read by the observer.
fn current_cluster() -> Value {
    json!({
        "apiVersion": "provisioning.dpu.nvidia.com/v1alpha1", "kind": "DPUCluster",
        "metadata": {"name": "carbide-dpf-cluster", "namespace": NS, "uid": "current-cluster-uid"},
        "spec": {"type": "static", "kubeconfig": "must-not-read-admin"},
    })
}

/// Connects the real repository to a mock management API without making external requests.
fn fixture() -> (KubeRepository, ApiHandle) {
    let (service, handle) = mock::pair();
    (
        KubeRepository::with_client(Client::new(service, NS)),
        handle,
    )
}

/// Supplies DPF's inline kubeconfig so tests can inspect routing without administrator credentials.
fn kubeconfig() -> Value {
    json!({
        "apiVersion": "v1", "kind": "Config",
        "current-context": OBSERVER_ACCOUNT,
        "clusters": [{"name": "remote", "cluster": {
            "server": "https://remote-dpu-cluster.example:6443",
            "certificate-authority-data": ByteString(include_bytes!("../../../../../dev/certs/localhost/ca.crt").to_vec()),
        }}],
        "users": [{"name": OBSERVER_ACCOUNT, "user": {"token": "observer-token"}}],
        "contexts": [{"name": OBSERVER_ACCOUNT, "context": {
            "cluster": "remote", "user": OBSERVER_ACCOUNT, "namespace": NS,
        }}],
    })
}

/// Supplies completed DPF credential issuance whose conditions acknowledge the current generation.
fn credential_request() -> Value {
    json!({
        "apiVersion": "svc.dpu.nvidia.com/v1alpha1", "kind": "DPUServiceCredentialRequest",
        "metadata": {"name": NAME, "namespace": NS, "uid": "request-uid", "resourceVersion": "5", "generation": 2},
        "spec": {
            "type": "kubeconfig", "targetCluster": {"name": "carbide-dpf-cluster", "namespace": NS},
            "serviceAccount": {"name": OBSERVER_ACCOUNT, "namespace": NS},
            "secret": {"name": NAME, "namespace": NS},
        },
        "status": {
            "targetClusterUID": "current-cluster-uid",
            "targetCluster": format!("{NS}/carbide-dpf-cluster"),
            "serviceAccount": format!("{NS}/{OBSERVER_ACCOUNT}"), "secret": format!("{NS}/{NAME}"),
            // Keep this token within the one-hour lifetime configured by the Helm request.
            "expirationTimestamp": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            "conditions": (["ServiceAccountReconciled", "SecretReconciled", "Ready"].map(|name| json!({
                "type": name, "status": "True", "observedGeneration": 2,
                "reason": "Success", "message": "", "lastTransitionTime": "2026-11-06T00:00:00Z",
            }))),
        },
    })
}

/// Supplies only the named observer Secret, including DPF's credential-request management labels.
fn credential_secret() -> Value {
    json!({
        "apiVersion": "v1", "kind": "Secret",
        "metadata": {
            "name": NAME, "namespace": NS, "uid": "secret-uid", "resourceVersion": "7",
            "labels": {
                "svc.dpu.nvidia.com/managed-by-controller": "dpuservicecredentialrequestcontroller",
                "svc.dpu.nvidia.com/credential-request-name": NAME,
                "svc.dpu.nvidia.com/credential-request-namespace": NS,
            },
        },
        "data": {"kubeconfig": ByteString(serde_json::to_vec(&kubeconfig()).expect("kubeconfig JSON"))},
    })
}

/// Answers exactly one expected GET, proving both the credential scope and remote list target.
async fn respond(handle: &mut ApiHandle, path: &str, status: u16, body: Value) -> http::Uri {
    let (request, sender) = handle.next_request().await.expect("expected API request");
    assert_eq!(request.method(), http::Method::GET);
    assert_eq!(request.uri().path(), path);
    let uri = request.uri().clone();
    sender.send_response(
        Response::builder()
            .status(status)
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string().into_bytes()))
            .expect("API response"),
    );
    uri
}

/// Compares meaningful list parameters independently of separator placement or query ordering.
fn list_query(uri: &http::Uri) -> BTreeSet<&str> {
    uri.query()
        .unwrap_or_default()
        .split('&')
        .filter(|part| !part.is_empty())
        .collect()
}

/// Bounds mock exchanges so a missing or unexpected request fails instead of hanging the test.
async fn together<T>(operation: impl Future<Output = T>, server: impl Future<Output = ()>) -> T {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (result, ()) = tokio::join!(operation, server);
        result
    })
    .await
    .expect("observer exchange must finish")
}

/// Rejects expired, mismatched or stale-generation credentials before reading their Secret, so
/// cluster recreation and delayed DPF status cannot route observations through old credentials.
#[tokio::test]
async fn credentials_require_current_cluster_identity_and_issuance() {
    let cases = [
        // A changed management UID must stop lookup before even reading a credential request.
        (
            "replaced management cluster",
            true,
            "/metadata/uid",
            json!("replacement-cluster-uid"),
        ),
        // A recreated DPUCluster must not reuse tokens issued for the prior UID.
        (
            "old cluster UID",
            false,
            "/status/targetClusterUID",
            json!("old-cluster-uid"),
        ),
        // A credential request for another account must not grant administrator access.
        (
            "wrong account",
            false,
            "/spec/serviceAccount/name",
            json!("administrator"),
        ),
        // A credential request must not point at the cluster administrator's Secret.
        (
            "admin secret",
            false,
            "/spec/secret/name",
            json!("admin-kubeconfig"),
        ),
        // A true condition for an older request generation is not current issuance.
        (
            "stale Secret condition",
            false,
            "/status/conditions/1/observedGeneration",
            json!(1),
        ),
        // Missing per-condition generation must not be replaced with top-level Ready.
        (
            "missing ServiceAccount generation",
            false,
            "/status/conditions/0/observedGeneration",
            Value::Null,
        ),
        // Completed reconciliation cannot make an expired token usable.
        (
            "expired token",
            false,
            "/status/expirationTimestamp",
            json!("2000-01-01T00:00:00Z"),
        ),
        // The generated CRD field is a string; malformed expiration must not produce usable credentials.
        (
            "malformed expiration",
            false,
            "/status/expirationTimestamp",
            json!("not-a-timestamp"),
        ),
        // Finalizing credential requests cannot authorize new observation work.
        (
            "deleting request",
            false,
            "/metadata/deletionTimestamp",
            json!("2026-11-06T00:00:00Z"),
        ),
    ];
    check_cases_async(
        cases.map(|(scenario, changes_cluster, path, value)| Case {
            scenario,
            input: (changes_cluster, path, value),
            expect: Yields(()),
        }),
        |(changes_cluster, path, value)| async move {
            // Change only the selected cluster or issuance field; all other validation inputs stay current.
            let (repo, mut handle) = fixture();
            let mut current_cluster = current_cluster();
            let mut request = credential_request();
            if changes_cluster {
                *current_cluster.pointer_mut(path).expect("cluster field") = value;
            } else if path == "/metadata/deletionTimestamp" {
                request["metadata"]["deletionTimestamp"] = value;
            } else {
                *request.pointer_mut(path).expect("case field") = value;
            }

            // Attempt remote access through the real credential lookup.
            let result = together(
                repo.service_vpc_cluster_reader(&cluster_identity()),
                async {
                    respond(&mut handle, CLUSTER_PATH, 200, current_cluster).await;
                    // A stale management UID must stop lookup before fetching its credential request.
                    if !changes_cluster {
                        respond(&mut handle, REQUEST_PATH, 200, request).await;
                    }
                },
            )
            .await;

            // Validation must fail without touching any Secret or remote resource.
            assert!(
                handle.next_request().now_or_never().is_none(),
                "unexpected credential request"
            );
            match result {
                Err(DpfError::InvalidState(_)) => Ok(()),
                _ => Err(()),
            }
        },
    )
    .await;
}

/// Reloads the named DPF Secret on each batch so rotation is consumed without ever reading the
/// DPUCluster's administrator kubeconfig.
#[tokio::test]
async fn observer_credentials_are_reloaded_without_admin_fallback() {
    // The DPUCluster names an admin Secret only as a sentinel that must never be requested.
    // Reuse an installed provider when another test initialized TLS first.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();
    let (repo, mut handle) = fixture();
    for token in [
        "observer-token-before-rotation",
        "observer-token-after-rotation",
    ] {
        let mut config = kubeconfig();
        config["users"][0]["user"]["token"] = json!(token);
        let mut secret = credential_secret();
        secret["data"]["kubeconfig"] = serde_json::to_value(ByteString(
            serde_json::to_vec(&config).expect("rotated kubeconfig JSON"),
        ))
        .expect("base64 credential");

        // Open a fresh reader through each full management lookup.
        together(
            repo.service_vpc_cluster_reader(&cluster_identity()),
            async {
                respond(&mut handle, CLUSTER_PATH, 200, current_cluster()).await;
                respond(&mut handle, REQUEST_PATH, 200, credential_request()).await;
                respond(&mut handle, SECRET_PATH, 200, secret).await;
            },
        )
        .await
        .expect("current observer credentials");
    }

    // The request sequence above permits exactly the configured observer credentials.
    assert!(
        handle.next_request().now_or_never().is_none(),
        "unexpected admin or remote request"
    );
}

/// Strips kubeconfig mechanisms outside DPF's inline CA/token contract so observer reads cannot
/// execute plugins, read local credential files or impersonate another account.
#[test]
fn observer_kubeconfig_uses_only_inline_dpf_credentials() {
    // Inline token and CA drive authentication; the other fields must not reach the client.
    let mut supplied = kubeconfig();
    supplied["users"][0]["user"]["exec"] = json!({"command": "must-not-execute"});
    supplied["users"][0]["user"]["as"] = json!("administrator");
    supplied["users"][0]["user"]["tokenFile"] = json!("/must-not-read");
    supplied["clusters"][0]["cluster"]["insecure-skip-tls-verify"] = json!(true);
    supplied["clusters"][0]["cluster"]["proxy-url"] = json!("http://must-not-use");

    // Select only the fields emitted by DPF.
    let selected =
        observer_kubeconfig(&serde_json::to_vec(&supplied).expect("kubeconfig JSON"), NS)
            .expect("select inline credentials");
    let selected = serde_json::to_value(selected).expect("selected kubeconfig JSON");

    // Exact output preserves the remote endpoint and token while removing every extra mechanism.
    assert_eq!(selected, kubeconfig());
}

/// Preserves every page and the list resourceVersion without cache or label selectors, preventing
/// a later-page or mislabeled child from being reported as absent.
#[tokio::test]
async fn service_chain_lists_preserve_authoritative_pagination() {
    // Connect the reader directly to a remote API with one child on each page.
    let (repo, mut handle) = fixture();
    let reader = ServiceVpcClusterReader {
        client: repo.client().clone(),
    };
    let path = format!("/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{NS}/servicechains");
    let child = |name: &str| {
        json!({
            "metadata": {"name": name, "uid": name},
            "spec": {"node": "target-node", "switches": [{"ports": [{"serviceInterface": {"matchLabels": {"interface": "test"}}}]}]},
        })
    };

    // Read through the real API, including the continuation request.
    let result = together(reader.list_service_chains(NS), async {
        let first = respond(&mut handle, &path, 200, json!({
            "metadata": {"resourceVersion": "42", "continue": "next-page"}, "items": [child("first")],
        })).await;
        let second = respond(&mut handle, &path, 200, json!({
            "metadata": {"resourceVersion": "42"}, "items": [child("second")],
        })).await;

        // An unset resourceVersion requests the most recent snapshot; selectors could hide leftovers.
        assert_eq!(list_query(&first), BTreeSet::from(["limit=500"]));
        assert_eq!(list_query(&second), BTreeSet::from(["limit=500", "continue=next-page"]));
    }).await.expect("complete authoritative list");

    // Both children and their common snapshot survive pagination.
    assert_eq!(
        result
            .items
            .iter()
            .map(|child| child.metadata.name.as_deref())
            .collect::<Vec<_>>(),
        [Some("first"), Some("second")]
    );
    assert_eq!(result.metadata.resource_version.as_deref(), Some("42"));
    assert_eq!(result.metadata.continue_, None);
}

/// Rejects changed snapshots and failed continuation pages instead of returning an empty or partial
/// list, preserving allocations when remote reads cannot prove absence.
#[tokio::test]
async fn incomplete_remote_lists_remain_errors() {
    let cases = [
        // A changed list version cannot describe one authoritative snapshot.
        (
            "changed snapshot",
            200,
            json!({"metadata": {"resourceVersion": "43"}, "items": []}),
        ),
        // Permission denial on a later page must not discard already-read children as success.
        (
            "permission denied",
            403,
            json!({"apiVersion": "v1", "kind": "Status", "status": "Failure", "reason": "Forbidden", "message": "denied", "code": 403}),
        ),
        // An expired continuation must restart the whole proof rather than use a replacement token.
        (
            "expired continuation",
            410,
            json!({"apiVersion": "v1", "kind": "Status", "status": "Failure", "reason": "Expired", "message": "expired", "code": 410}),
        ),
    ];
    check_cases_async(
        cases.map(|(scenario, status, page)| Case {
            scenario,
            input: (status, page),
            expect: Fails,
        }),
        |(status, page)| async move {
            // Keep the initial snapshot constant while varying the continuation failure.
            let (repo, mut handle) = fixture();
            let reader = ServiceVpcClusterReader {
                client: repo.client().clone(),
            };
            let path = "/api/v1/nodes";

            // Exercise the same pagination helper through a Node list.
            let result = together(reader.list_nodes(), async {
                respond(
                    &mut handle,
                    path,
                    200,
                    json!({
                        "metadata": {"resourceVersion": "42", "continue": "next-page"},
                        "items": [{"metadata": {"name": "target", "uid": "node-uid"}}],
                    }),
                )
                .await;
                respond(&mut handle, path, status, page).await;
            })
            .await;

            // A failed proof cannot supply a successful empty Node set.
            result.map(drop).map_err(drop)
        },
    )
    .await;
}

/// Applies one ten-second deadline to all pages, so a stalled later page cannot hang a controller
/// iteration after a successful first read. Time spent on the first page reduces the continuation's
/// remaining budget rather than starting a new ten-second attempt.
#[tokio::test(start_paused = true)]
async fn complete_list_deadline_includes_later_pages() {
    // Spend four seconds on the first page before leaving its continuation unanswered.
    let (repo, mut handle) = fixture();
    let reader = ServiceVpcClusterReader {
        client: repo.client().clone(),
    };
    let list_started = tokio::time::Instant::now();
    let operation =
        tokio::time::timeout(Duration::from_secs(11), reader.list_service_chain_sets(NS));
    tokio::pin!(operation);
    let path = format!("/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{NS}/servicechainsets");
    let server = async {
        let (request, sender) = handle.next_request().await.expect("first page request");
        assert_eq!(request.method(), http::Method::GET);
        assert_eq!(request.uri().path(), path);
        tokio::time::advance(Duration::from_secs(4)).await;
        sender.send_response(
            Response::builder()
                .status(200)
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "metadata": {
                            "resourceVersion": "42",
                            "continue": "next-page", // Forces a second page inside the same budget.
                        },
                        "items": [],
                    })
                    .to_string()
                    .into_bytes(),
                ))
                .expect("first page response"),
        );
        let (request, sender) = handle.next_request().await.expect("continuation request");
        assert_eq!(request.method(), http::Method::GET);
        assert_eq!(request.uri().path(), path);
        assert_eq!(
            list_query(request.uri()),
            BTreeSet::from(["limit=500", "continue=next-page"])
        );
        sender
    };

    // Require the continuation to start before checking its remaining six-second budget.
    let pending_response = tokio::select! {
        sender = server => sender,
        _ = &mut operation => panic!("list completed before the continuation stalled"),
    };
    assert_eq!(list_started.elapsed(), Duration::from_secs(4));
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(operation.as_mut().now_or_never().is_none());
    tokio::time::advance(Duration::from_secs(1)).await;
    let result = operation
        .await
        .expect("list deadline must precede the eleven-second harness deadline");

    // Exact elapsed time rejects per-page timer resets even when paused time advances automatically.
    assert!(matches!(result, Err(DpfError::Timeout { .. })));
    assert_eq!(list_started.elapsed(), Duration::from_secs(10));
    tokio::task::yield_now().await;
    assert!(
        handle.next_request().now_or_never().is_none(),
        "unexpected list retry"
    );
    drop(pending_response);
}

/// Propagates unavailable credentials and management read failures without administrator fallback,
/// preventing missing permissions or an unready credential request from becoming empty observations.
#[tokio::test]
async fn unavailable_observer_credentials_remain_errors() {
    let cases = [
        // A missing request means observer access has not been provisioned.
        ("unprovisioned observer", 1, 404),
        // A denied Secret read must not fall back to the DPUCluster's administrator Secret.
        ("Secret access denied", 2, 403),
        // An unavailable management API cannot establish current cluster identity.
        ("cluster lookup unavailable", 0, 503),
    ];
    check_cases_async(cases.map(|(scenario, failed_step, status)| Case {
        scenario, input: (failed_step, status), expect: Fails,
    }), |(failed_step, status)| async move {
        // Serve each prerequisite until the selected management read fails.
        let (repo, mut handle) = fixture();
        let paths = [CLUSTER_PATH, REQUEST_PATH, SECRET_PATH];
        let result = together(repo.service_vpc_cluster_reader(&cluster_identity()), async {
            for (step, path) in paths.iter().enumerate().take(failed_step + 1) {
                let response = if step == failed_step {
                    json!({"apiVersion": "v1", "kind": "Status", "status": "Failure",
                        "reason": "Unavailable", "message": "observer lookup failed", "code": status})
                } else if step == 0 {
                    current_cluster()
                } else {
                    credential_request()
                };
                respond(&mut handle, path, if step == failed_step { status } else { 200 }, response).await;
            }
        }).await;

        // Failed lookups must stop without an extra credential or administrator request.
        assert!(handle.next_request().now_or_never().is_none(), "unexpected fallback request");
        result.map(drop).map_err(drop)
    }).await;
}

/// Includes the final credential Secret read in the ten-second lookup deadline, so an unavailable
/// Secret cannot hang a controller after the cluster and credential-request checks have succeeded.
/// Time spent reading the cluster reduces the Secret's remaining budget rather than resetting it.
#[tokio::test(start_paused = true)]
async fn credential_lookup_deadline_includes_secret_read() {
    // Spend four seconds on the cluster read, then stall the named Secret after valid issuance.
    let (repo, mut handle) = fixture();
    let cluster = cluster_identity();
    let lookup_started = tokio::time::Instant::now();
    let operation = tokio::time::timeout(
        Duration::from_secs(11),
        repo.service_vpc_cluster_reader(&cluster),
    );
    tokio::pin!(operation);
    let server = async {
        let (request, sender) = handle.next_request().await.expect("cluster request");
        assert_eq!(request.method(), http::Method::GET);
        assert_eq!(request.uri().path(), CLUSTER_PATH);
        tokio::time::advance(Duration::from_secs(4)).await;
        sender.send_response(
            Response::builder()
                .status(200)
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(current_cluster().to_string().into_bytes()))
                .expect("cluster response"),
        );
        respond(&mut handle, REQUEST_PATH, 200, credential_request()).await;
        let (request, sender) = handle.next_request().await.expect("Secret request");
        assert_eq!(request.method(), http::Method::GET);
        assert_eq!(request.uri().path(), SECRET_PATH);
        sender
    };

    // Require the Secret read to start before checking its remaining six-second budget.
    let pending_response = tokio::select! {
        sender = server => sender,
        _ = &mut operation => panic!("lookup completed before the Secret stalled"),
    };
    assert_eq!(lookup_started.elapsed(), Duration::from_secs(4));
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(operation.as_mut().now_or_never().is_none());
    tokio::time::advance(Duration::from_secs(1)).await;
    let result = operation
        .await
        .expect("lookup deadline must precede the eleven-second harness deadline");

    // Exact elapsed time rejects per-read timer resets without allowing retries or another credential source.
    assert!(matches!(result, Err(DpfError::Timeout { .. })));
    assert_eq!(lookup_started.elapsed(), Duration::from_secs(10));
    tokio::task::yield_now().await;
    assert!(
        handle.next_request().now_or_never().is_none(),
        "unexpected credential retry or fallback"
    );
    drop(pending_response);
}

/// Rejects lists without snapshot identity even when they contain no items, preventing an incomplete
/// API response from being accepted as authoritative absence.
#[tokio::test]
async fn lists_require_snapshot_identity_before_accepting_absence() {
    // Return an empty API list without the resourceVersion needed to identify its snapshot.
    let (repo, mut handle) = fixture();
    let reader = ServiceVpcClusterReader {
        client: repo.client().clone(),
    };

    // Exercise the same read used to establish Node presence and identity.
    let result = together(reader.list_nodes(), async {
        respond(
            &mut handle,
            "/api/v1/nodes",
            200,
            json!({"metadata": {}, "items": []}),
        )
        .await;
    })
    .await;

    // An incomplete list must remain an error even when no continuation is present.
    assert!(matches!(result, Err(DpfError::InvalidState(_))));
}

/// Produces Helm's credential names for namespaced DPUClusters so reads match the chart's named Secret grants.
#[test]
fn observer_credential_names_match_helm_identity() {
    check_values(
        [
            // The ordinary deployment must read exactly the Secret authorized by the API chart.
            Check {
                scenario: "default namespace",
                input: cluster_identity(),
                expect: "nico-service-vpc-observer-19205ee90d2f8fe2".to_owned(),
            },
            // Same-name clusters in another namespace must not share a credential name.
            Check {
                scenario: "other namespace",
                input: DpuClusterIdentity {
                    namespace: "other-dpf".to_owned(),
                    ..cluster_identity()
                },
                expect: "nico-service-vpc-observer-330d35817d6aa2ad".to_owned(),
            },
        ],
        |cluster| observer_credential_name(&cluster),
    );
}

/// Rejects unexpected continuations from management inventories so neither DPU routing nor
/// registration revalidation can silently consume only part of a namespace.
#[tokio::test]
async fn management_inventories_reject_incomplete_reads() {
    let cases = [
        // A partial DPU list could hide an ambiguous or replaced management mapping.
        (
            "DPU mappings",
            true,
            "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/dpf-operator-system/dpus",
        ),
        // A partial registration list could make a current exclusion appear missing.
        (
            "chain registrations",
            false,
            "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/dpf-operator-system/dpuservicechains",
        ),
    ];
    check_cases_async(
        cases.map(|(scenario, lists_dpus, path)| Case {
            scenario,
            input: (lists_dpus, path),
            expect: Fails,
        }),
        |(lists_dpus, path)| async move {
            // Return an incomplete list even though the caller did not request pagination.
            let (repo, mut handle) = fixture();
            let operation = async {
                if lists_dpus {
                    crate::repository::DpuRepository::list(&repo, NS, None)
                        .await
                        .map(drop)
                } else {
                    crate::repository::DpuServiceChainRepository::list(&repo, NS, None)
                        .await
                        .map(drop)
                }
            };

            // Exercise each actual management repository operation.
            let result = together(operation, async {
                respond(&mut handle, path, 200, json!({
                "metadata": {"resourceVersion": "42", "continue": "unread-page"}, "items": [],
            })).await;
            })
            .await;

            // Continuation must remain an error instead of returning an empty complete inventory.
            result.map_err(drop)
        },
    )
    .await;
}
