/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use http::{Request, Response};
use kube::Client;
use kube::client::Body;
use serde_json::{Value, json};
use tower_test::mock;

use super::*;
use crate::KubeRepository;
use crate::sdk::NoLabels;
use crate::types::DpuDeviceInfo;

const NS: &str = "test-ns";
const DEVICE: &str = "01-02-03-04-05-06";
const DEVICE_PATH: &str = "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices/device-01-02-03-04-05-06";
type ApiHandle = mock::Handle<Request<Body>, Response<Body>>;

fn fixture() -> (DpfSdk<KubeRepository>, ApiHandle) {
    let (service, handle) = mock::pair();
    let sdk = DpfSdk {
        repo: Arc::new(KubeRepository::with_client(Client::new(service, "default"))),
        namespace: NS.to_string(),
        labeler: NoLabels,
        operator_version: semver::Version::new(26, 8, 0),
        shared_bmc_password_ready: Arc::new(AtomicBool::new(false)),
        _bmc_refresh_guard: None,
    };
    (sdk, handle)
}

async fn together<T>(operation: impl Future<Output = T>, server: impl Future<Output = ()>) -> T {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (result, ()) = tokio::join!(operation, server);
        result
    })
    .await
    .expect("mock Kubernetes exchange must complete")
}

async fn exchange(
    handle: &mut ApiHandle,
    method: &str,
    path: &str,
    code: u16,
    response: Value,
) -> Value {
    let (request, sender) = handle
        .next_request()
        .await
        .expect("expected Kubernetes request");
    assert_eq!(request.method().as_str(), method);
    assert_eq!(request.uri().path(), path);
    if method == "GET" && path == "/api/v1/namespaces/test-ns/secrets" {
        assert!(
            request
                .uri()
                .query()
                .is_some_and(|query| query.contains("labelSelector="))
        );
    }
    let bytes = request.into_body().collect_bytes().await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    sender.send_response(
        Response::builder()
            .status(code)
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(response.to_string().into_bytes()))
            .unwrap(),
    );
    body
}

fn missing() -> Value {
    json!({"apiVersion": "v1", "kind": "Status", "status": "Failure", "reason": "NotFound", "message": "missing", "code": 404})
}

fn secret_path(name: &str) -> String {
    format!("/api/v1/namespaces/{NS}/secrets/{name}")
}

const NODE_PATH: &str =
    "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpunodes/node-host";
const DPUS_PATH: &str = "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpus";
const SECRETS_PATH: &str = "/api/v1/namespaces/test-ns/secrets";

async fn drained_host(handle: &mut ApiHandle) {
    exchange(handle, "GET", NODE_PATH, 404, missing()).await;
    exchange(handle, "GET", DEVICE_PATH, 404, missing()).await;
    exchange(handle, "GET", DPUS_PATH, 200, json!({"kind": "DPUList", "apiVersion": "provisioning.dpu.nvidia.com/v1alpha1", "items": []})).await;
}

fn secret_list(items: Vec<Value>) -> Value {
    json!({"kind": "SecretList", "apiVersion": "v1", "items": items})
}

#[tokio::test]
async fn new_device_credential_creation_rejects_even_identical_existing_secrets() {
    for version in [Some(3), None] {
        let (sdk, mut handle) = fixture();
        let name = bmc_credential_secret_name(DEVICE, version);
        let operation = async {
            assert_eq!(
                sdk.create_bmc_credential_secret(DEVICE, version, "password")
                    .await
                    .unwrap(),
                name
            );
            assert!(
                sdk.create_bmc_credential_secret(DEVICE, version, "password")
                    .await
                    .is_err()
            );
        };
        let server = async {
            let body = exchange(
                &mut handle,
                "POST",
                SECRETS_PATH,
                201,
                secret(version, "password"),
            )
            .await;
            assert_eq!(body["metadata"]["name"], name);
            assert_eq!(body["immutable"], true);
            assert_eq!(body["data"], secret(version, "password")["data"]);
            assert_eq!(
                body["metadata"]["labels"],
                secret(version, "password")["metadata"]["labels"]
            );
            exchange(&mut handle, "POST", SECRETS_PATH, 409,
                json!({"kind": "Status", "apiVersion": "v1", "status": "Failure", "reason": "AlreadyExists", "code": 409})).await;
        };
        together(operation, server).await;
    }
}

#[tokio::test]
async fn host_credential_cleanup_waits_for_resources_and_secret_finalizers() {
    let (sdk, mut handle) = fixture();
    let initial_name = bmc_credential_secret_name(DEVICE, None);
    let versioned_name = bmc_credential_secret_name(DEVICE, Some(1));
    let operation = async {
        for complete in [false, false, false, false, false, true] {
            assert_eq!(
                sdk.delete_host_bmc_credential_secrets("host", &[DEVICE.to_string()])
                    .await
                    .unwrap(),
                complete
            );
        }
    };
    let server = async {
        exchange(&mut handle, "GET", NODE_PATH, 200, json!({"apiVersion": "provisioning.dpu.nvidia.com/v1alpha1", "kind": "DPUNode", "metadata": {"name": "node-host"}, "spec": {}})).await;

        exchange(&mut handle, "GET", NODE_PATH, 404, missing()).await;
        let mut deleting_device = device(Some(&initial_name), None, "Unknown", "Pending");
        deleting_device["metadata"]["deletionTimestamp"] = json!("2026-11-10T00:00:00Z");
        exchange(&mut handle, "GET", DEVICE_PATH, 200, deleting_device).await;

        exchange(&mut handle, "GET", NODE_PATH, 404, missing()).await;
        exchange(&mut handle, "GET", DEVICE_PATH, 404, missing()).await;
        exchange(&mut handle, "GET", DPUS_PATH, 200, json!({"apiVersion": "provisioning.dpu.nvidia.com/v1alpha1", "kind": "DPUList", "items": [{
            "metadata": {"name": "still-deleting"},
            "spec": {"dpuNodeName": "node-host", "dpuDeviceName": dpu_device_cr_name(DEVICE), "dpuFlavor": "test", "nodeEffect": {}, "serialNumber": "serial-1"},
        }]})).await;

        drained_host(&mut handle).await;
        exchange(
            &mut handle,
            "GET",
            SECRETS_PATH,
            200,
            secret_list(vec![secret(None, "initial"), secret(Some(1), "v1")]),
        )
        .await;
        for name in [&initial_name, &versioned_name] {
            let body = exchange(
                &mut handle,
                "DELETE",
                &secret_path(name),
                200,
                json!({"kind": "Status", "apiVersion": "v1", "status": "Success", "code": 200}),
            )
            .await;
            assert_eq!(
                body,
                json!({"preconditions": {"uid": "secret-uid", "resourceVersion": "9"}})
            );
        }

        drained_host(&mut handle).await;
        let mut deleting_secret = secret(None, "initial");
        deleting_secret["metadata"]["deletionTimestamp"] = json!("2026-11-10T00:00:00Z");
        exchange(
            &mut handle,
            "GET",
            SECRETS_PATH,
            200,
            secret_list(vec![deleting_secret]),
        )
        .await;

        drained_host(&mut handle).await;
        exchange(&mut handle, "GET", SECRETS_PATH, 200, secret_list(vec![])).await;
    };
    together(operation, server).await;
}

#[tokio::test]
async fn host_credential_cleanup_protects_foreign_shared_and_replaced_secrets() {
    for scenario in ["foreign", "shared", "missing UID", "delete conflict"] {
        let (sdk, mut handle) = fixture();
        let name = bmc_credential_secret_name(DEVICE, None);
        let device_ids = [DEVICE.to_string()];
        let operation = sdk.delete_host_bmc_credential_secrets("host", &device_ids);
        let server = async {
            drained_host(&mut handle).await;
            let mut stored = secret(None, "password");
            match scenario {
                "foreign" => stored["metadata"]["labels"][DEVICE_LABEL] = json!("another-device"),
                "shared" => stored["metadata"]["name"] = json!(SECRET_NAME),
                "missing UID" => {
                    assert!(
                        stored["metadata"]
                            .as_object_mut()
                            .unwrap()
                            .remove("uid")
                            .is_some()
                    );
                }
                _ => {}
            }
            exchange(
                &mut handle,
                "GET",
                SECRETS_PATH,
                200,
                secret_list(vec![stored]),
            )
            .await;
            if scenario == "delete conflict" {
                exchange(&mut handle, "DELETE", &secret_path(&name), 409,
                    json!({"kind": "Status", "apiVersion": "v1", "status": "Failure", "reason": "Conflict", "code": 409})).await;
            }
        };
        let result = together(operation, server).await;
        if scenario == "foreign" {
            assert!(result.unwrap());
        } else {
            assert!(result.is_err(), "{scenario}");
        }
    }
}

#[tokio::test]
async fn host_credential_cleanup_is_a_noop_for_legacy_dpf() {
    let (mut sdk, _handle) = fixture();
    sdk.operator_version = semver::Version::new(26, 7, 0);
    assert!(
        together(
            sdk.delete_host_bmc_credential_secrets("host", &[DEVICE.to_string()]),
            async {}
        )
        .await
        .unwrap()
    );
}

fn secret(version: Option<u32>, password: &str) -> Value {
    serde_json::to_value(Secret {
        metadata: ObjectMeta {
            name: Some(bmc_credential_secret_name(DEVICE, version)),
            namespace: Some(NS.to_string()),
            uid: Some("secret-uid".to_string()),
            resource_version: Some("9".to_string()),
            labels: Some(BTreeMap::from([
                (DEVICE_LABEL.to_string(), DEVICE.to_string()),
                (
                    VERSION_LABEL.to_string(),
                    version.map_or_else(|| "initial".to_string(), |v| v.to_string()),
                ),
            ])),
            finalizers: Some(vec![
                "provisioning.dpu.nvidia.com/bmc-credential".to_string(),
            ]),
            ..Default::default()
        },
        immutable: Some(true),
        type_: Some("Opaque".to_string()),
        data: Some(BTreeMap::from([(
            "password".to_string(),
            ByteString(password.as_bytes().to_vec()),
        )])),
        ..Default::default()
    })
    .unwrap()
}

fn device(
    desired: Option<&str>,
    active: Option<&str>,
    condition_status: &str,
    reason: &str,
) -> Value {
    json!({
        "apiVersion": "provisioning.dpu.nvidia.com/v1alpha1", "kind": "DPUDevice",
        "metadata": { "name": dpu_device_cr_name(DEVICE), "namespace": NS, "uid": "device-uid", "resourceVersion": "17", "generation": 2 },
        "spec": { "serialNumber": "serial-1", "bmcCredentialSecretName": desired },
        "status": {
            "bmcCredentialSecretName": active,
            "conditions": [{ "type": "BMCCredentialsReady", "status": condition_status, "reason": reason,
                "message": "password=must-not-appear-in-observation", "observedGeneration": 2,
                "lastTransitionTime": "2026-10-27T00:00:00Z" }],
        },
    })
}

#[test]
fn bmc_credential_names_are_stable_and_distinguish_device_and_revision() {
    let name = bmc_credential_secret_name(DEVICE, Some(u32::MAX));
    assert_eq!(name, "01-02-03-04-05-06-v4294967295");
    assert_ne!(
        name,
        bmc_credential_secret_name("01-02-03-04-05-07", Some(u32::MAX))
    );
    assert_ne!(name, bmc_credential_secret_name(DEVICE, Some(0)));
    assert_eq!(
        bmc_credential_secret_name(DEVICE, None),
        format!("{DEVICE}-initial")
    );
    assert!(name.len() <= 63);
    assert!(
        name.bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    );
}

#[tokio::test]
async fn bmc_credential_secret_is_created_immutable_and_retries_without_overwrite() {
    for version in [Some(3), None] {
        let (sdk, mut handle) = fixture();
        let name = bmc_credential_secret_name(DEVICE, version);
        let stored = secret(version, "new-password");
        let operation = async {
            assert_eq!(
                sdk.ensure_bmc_credential_secret(DEVICE, version, "new-password")
                    .await
                    .unwrap(),
                name
            );
            assert_eq!(
                sdk.ensure_bmc_credential_secret(DEVICE, version, "new-password")
                    .await
                    .unwrap(),
                name
            );
            let error = sdk
                .ensure_bmc_credential_secret(DEVICE, version, "different-password")
                .await
                .unwrap_err();
            assert!(!format!("{error:?}").contains("different-password"));
        };
        let server = async {
            let path = secret_path(&name);
            exchange(&mut handle, "GET", &path, 404, missing()).await;
            let created = exchange(
                &mut handle,
                "POST",
                "/api/v1/namespaces/test-ns/secrets",
                201,
                stored.clone(),
            )
            .await;
            assert_eq!(created["immutable"], true);
            assert_eq!(created["metadata"]["name"], name);
            assert_eq!(created["metadata"]["namespace"], NS);
            assert_eq!(created["data"], stored["data"]);
            assert_eq!(created["metadata"]["labels"], stored["metadata"]["labels"]);
            for _ in 0..3 {
                exchange(&mut handle, "GET", &path, 200, stored.clone()).await;
            }
        };
        together(operation, server).await;
    }
}

#[tokio::test]
async fn bmc_credential_create_race_validates_the_winning_secret() {
    for (scenario, password, mutable, success) in [
        ("identical concurrent create", "new-password", false, true),
        ("wrong version label", "new-password", false, false),
        (
            "different credential at the same revision",
            "other-password",
            false,
            false,
        ),
        (
            "mutable credential cannot be adopted",
            "new-password",
            true,
            false,
        ),
        (
            "different Secret type cannot be adopted",
            "new-password",
            false,
            false,
        ),
    ] {
        let (sdk, mut handle) = fixture();
        let mut stored = secret(Some(1), password);
        stored["immutable"] = json!(!mutable);
        if scenario == "different Secret type cannot be adopted" {
            stored["type"] = json!("kubernetes.io/basic-auth");
        }
        if scenario == "wrong version label" {
            stored["metadata"]["labels"][VERSION_LABEL] = json!("2");
        }
        let operation = sdk.ensure_bmc_credential_secret(DEVICE, Some(1), "new-password");
        let server = async {
            let path = secret_path(&bmc_credential_secret_name(DEVICE, Some(1)));
            exchange(&mut handle, "GET", &path, 404, missing()).await;
            exchange(&mut handle, "POST", "/api/v1/namespaces/test-ns/secrets", 409,
                json!({"status": "Failure", "reason": "AlreadyExists", "message": "already exists", "code": 409})).await;
            exchange(&mut handle, "GET", &path, 200, stored).await;
        };
        let result = together(operation, server).await;
        assert_eq!(result.is_ok(), success, "{scenario}");
    }
}

#[tokio::test]
async fn bmc_credential_observation_distinguishes_adoption_pending_and_dpf_failures() {
    let mut cases = vec![
        (
            "adopted",
            device(Some("new"), Some("new"), "True", "Success"),
            BmcCredentialHealth::Ready,
            true,
        ),
        (
            "old credential still active",
            device(Some("new"), Some("old"), "True", "Success"),
            BmcCredentialHealth::Ready,
            false,
        ),
        (
            "unknown health",
            device(Some("new"), Some("new"), "Unknown", "Reconciling"),
            BmcCredentialHealth::Pending,
            false,
        ),
    ];
    let mut absent = device(Some("new"), None, "True", "Success");
    absent["status"]
        .as_object_mut()
        .unwrap()
        .remove("conditions");
    cases.push((
        "condition missing",
        absent,
        BmcCredentialHealth::Pending,
        false,
    ));
    let mut stale = device(Some("new"), Some("new"), "True", "Success");
    stale["status"]["conditions"][0]["observedGeneration"] = json!(1);
    cases.push((
        "stale condition",
        stale,
        BmcCredentialHealth::Pending,
        false,
    ));
    for (reason, expected) in [
        (
            "CredentialsSecretNotFound",
            BmcCredentialFailureReason::CredentialsSecretNotFound,
        ),
        (
            "CredentialsSecretInvalid",
            BmcCredentialFailureReason::CredentialsSecretInvalid,
        ),
        (
            "BMCAuthenticationFailed",
            BmcCredentialFailureReason::BmcAuthenticationFailed,
        ),
        (
            "ModeSwitchNotAllowed",
            BmcCredentialFailureReason::ModeSwitchNotAllowed,
        ),
        (
            "SharedCredentialSecretNotAllowed",
            BmcCredentialFailureReason::SharedCredentialSecretNotAllowed,
        ),
        (
            "FutureDpfFailure",
            BmcCredentialFailureReason::Other("FutureDpfFailure".to_string()),
        ),
    ] {
        cases.push((
            reason,
            device(Some("new"), Some("old"), "False", reason),
            BmcCredentialHealth::Failed(expected),
            false,
        ));
    }
    for (scenario, resource, health, adopted) in cases {
        let (sdk, mut handle) = fixture();
        let observation = together(sdk.observe_bmc_credential(DEVICE), async {
            exchange(&mut handle, "GET", DEVICE_PATH, 200, resource).await;
        })
        .await
        .unwrap();
        assert_eq!(observation.health, health, "{scenario}");
        assert_eq!(observation.is_adopted("new"), adopted, "{scenario}");
        assert!(!format!("{observation:?}").contains("must-not-appear"));
    }
}

#[tokio::test]
async fn bmc_credential_reference_patch_is_fenced_and_conflict_is_observable() {
    let (sdk, mut handle) = fixture();
    let operation = async {
        let observation = sdk.observe_bmc_credential(DEVICE).await.unwrap();
        let error = sdk
            .set_bmc_credential_reference(&observation, "new")
            .await
            .unwrap_err();
        assert!(error.is_already_exists_or_conflict());
        let updated = sdk.observe_bmc_credential(DEVICE).await.unwrap();
        sdk.set_bmc_credential_reference(&updated, "new")
            .await
            .unwrap();
    };
    let server = async {
        let old = device(Some("old"), Some("old"), "True", "Success");
        exchange(&mut handle, "GET", DEVICE_PATH, 200, old.clone()).await;
        let patch = exchange(&mut handle, "PATCH", DEVICE_PATH, 409,
            json!({"status": "Failure", "reason": "Conflict", "message": "stale resource version", "code": 409})).await;
        assert_eq!(
            patch,
            json!({"metadata": {"uid": "device-uid", "resourceVersion": "17"}, "spec": {"bmcCredentialSecretName": "new"}})
        );
        let mut current = old;
        current["metadata"]["resourceVersion"] = json!("18");
        exchange(&mut handle, "GET", DEVICE_PATH, 200, current.clone()).await;
        current["spec"]["bmcCredentialSecretName"] = json!("new");
        let retry = exchange(&mut handle, "PATCH", DEVICE_PATH, 200, current).await;
        assert_eq!(retry["metadata"]["resourceVersion"], "18");
        assert_eq!(retry["metadata"]["uid"], "device-uid");
    };
    together(operation, server).await;
}

#[tokio::test]
async fn registration_with_a_credential_never_changes_an_existing_reference() {
    let (sdk, mut handle) = fixture();
    let name = bmc_credential_secret_name(DEVICE, Some(3));
    let info = DpuDeviceInfo {
        device_id: DEVICE.to_string(),
        dpu_bmc_ip: "192.0.2.1".parse().unwrap(),
        host_bmc_ip: "192.0.2.2".parse().unwrap(),
        serial_number: "serial-1".to_string(),
        dpu_machine_id: "machine-1".to_string(),
        is_primary: true,
    };
    let operation = async {
        let secret = sdk
            .ensure_bmc_credential_secret(DEVICE, Some(3), "established-password")
            .await
            .unwrap();
        assert!(
            sdk.register_dpu_device_with_credential(info.clone(), None, &secret)
                .await
                .is_err()
        );
        let secret = sdk
            .ensure_bmc_credential_secret(DEVICE, Some(3), "established-password")
            .await
            .unwrap();
        sdk.register_dpu_device_with_credential(info.clone(), None, &secret)
            .await
            .unwrap();
        sdk.register_dpu_device_with_credential(info.clone(), None, &secret)
            .await
            .unwrap();
        assert!(
            sdk.register_dpu_device_with_credential(info, None, "different")
                .await
                .is_err()
        );
    };
    let server = async {
        let stored = secret(Some(3), "established-password");
        let path = secret_path(&name);
        exchange(&mut handle, "GET", &path, 404, missing()).await;
        exchange(
            &mut handle,
            "POST",
            "/api/v1/namespaces/test-ns/secrets",
            201,
            stored.clone(),
        )
        .await;
        exchange(&mut handle, "GET", &path, 200, stored.clone()).await;
        exchange(&mut handle, "GET", DEVICE_PATH, 404, missing()).await;
        exchange(&mut handle, "POST", "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices", 503,
            json!({"status": "Failure", "reason": "ServiceUnavailable", "message": "retry", "code": 503})).await;
        exchange(&mut handle, "GET", &path, 200, stored).await;
        let created = device(Some(&name), None, "Unknown", "Reconciling");
        exchange(&mut handle, "GET", DEVICE_PATH, 404, missing()).await;
        let request = exchange(
            &mut handle,
            "POST",
            "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices",
            201,
            created.clone(),
        )
        .await;
        assert_eq!(request["spec"]["bmcCredentialSecretName"], name);
        for _ in 0..2 {
            exchange(&mut handle, "GET", DEVICE_PATH, 200, created.clone()).await;
        }
    };
    together(operation, server).await;
}

#[tokio::test]
async fn registration_create_race_checks_the_winning_credential_reference() {
    for (reference, success) in [(Some("new"), true), (None, false)] {
        let (sdk, mut handle) = fixture();
        let info = DpuDeviceInfo {
            device_id: DEVICE.to_string(),
            dpu_bmc_ip: "192.0.2.1".parse().unwrap(),
            host_bmc_ip: "192.0.2.2".parse().unwrap(),
            serial_number: "serial-1".to_string(),
            dpu_machine_id: "machine-1".to_string(),
            is_primary: true,
        };
        let result = together(
            sdk.register_dpu_device_with_credential(info, None, "new"),
            async {
                exchange(&mut handle, "GET", DEVICE_PATH, 404, missing()).await;
                exchange(&mut handle, "POST", "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices", 409,
                    json!({"status": "Failure", "reason": "AlreadyExists", "message": "concurrent create", "code": 409})).await;
                exchange(&mut handle, "GET", DEVICE_PATH, 200, device(reference, None, "Unknown", "Reconciling")).await;
            },
        ).await;
        assert_eq!(result.is_ok(), success, "winning reference: {reference:?}");
    }
}

#[tokio::test]
async fn old_bmc_credential_cleanup_preserves_active_foreign_and_replaced_secrets() {
    for scenario in [
        "delete",
        "not adopted",
        "foreign",
        "replacement conflict",
        "already missing",
    ] {
        let (sdk, mut handle) = fixture();
        let old_name = bmc_credential_secret_name(DEVICE, Some(0));
        let new_name = bmc_credential_secret_name(DEVICE, Some(1));
        let operation = sdk.delete_old_bmc_credential_secret(DEVICE, &old_name, &new_name);
        let server = async {
            let current = device(
                Some(&new_name),
                Some(if scenario == "not adopted" {
                    &old_name
                } else {
                    &new_name
                }),
                "True",
                "Success",
            );
            exchange(&mut handle, "GET", DEVICE_PATH, 200, current).await;
            if scenario == "not adopted" {
                return;
            }
            if scenario == "already missing" {
                exchange(&mut handle, "GET", &secret_path(&old_name), 404, missing()).await;
                return;
            }
            let mut old_secret = secret(Some(0), "old-password");
            if scenario == "foreign" {
                old_secret["metadata"]["labels"] = json!({});
            }
            exchange(&mut handle, "GET", &secret_path(&old_name), 200, old_secret).await;
            if scenario == "foreign" {
                return;
            }
            let conflict = scenario == "replacement conflict";
            let body = exchange(&mut handle, "DELETE", &secret_path(&old_name), if conflict { 409 } else { 200 },
                json!({"kind": "Status", "apiVersion": "v1", "status": if conflict { "Failure" } else { "Success" }, "reason": if conflict { "Conflict" } else { "Success" }, "message": "delete", "code": if conflict { 409 } else { 200 }})).await;
            assert_eq!(
                body,
                json!({"preconditions": {"uid": "secret-uid", "resourceVersion": "9"}})
            );
        };
        let result = together(operation, server).await;
        assert_eq!(
            result.is_ok(),
            matches!(scenario, "delete" | "already missing"),
            "{scenario}"
        );
    }
    let (sdk, _handle) = fixture();
    for protected in [SECRET_NAME, "adopted"] {
        assert!(
            sdk.delete_old_bmc_credential_secret(DEVICE, protected, "adopted")
                .await
                .is_err()
        );
    }
}
