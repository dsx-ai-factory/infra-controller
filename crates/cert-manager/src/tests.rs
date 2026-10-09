// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, Mutex};

use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use k8s_openapi::ByteString;
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower::service_fn;

use super::*;
use crate::resources::RequestCondition;

const MACHINE_ID: &str = "fm100xtest";
const SPIFFE: &str = "spiffe://nico.local/forge-system/machine/fm100xtest";

fn spiffe_identity() -> SpiffeIdentity {
    SpiffeIdentity {
        trust_domain: "nico.local".into(),
        machine_base_path: "/forge-system/machine/".into(),
    }
}

struct TestCa {
    pem: String,
    issuer: Issuer<'static, KeyPair>,
}

impl TestCa {
    fn new() -> Self {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        Self {
            pem,
            issuer: Issuer::new(params, key),
        }
    }

    fn sign(&self, csr: &[u8], lifetime: Duration) -> RequestStatus {
        let mut csr =
            CertificateSigningRequestParams::from_pem(std::str::from_utf8(csr).unwrap()).unwrap();
        csr.params.not_before = OffsetDateTime::now_utc() - time::Duration::minutes(5);
        csr.params.not_after =
            OffsetDateTime::now_utc() + time::Duration::seconds(lifetime.as_secs() as i64);
        RequestStatus {
            conditions: vec![condition("Ready", "True", "Issued")],
            certificate: Some(ByteString(
                csr.signed_by(&self.issuer).unwrap().pem().into_bytes(),
            )),
            ca: Some(ByteString(self.pem.as_bytes().to_vec())),
        }
    }
}

fn condition(type_: &str, status: &str, reason: &str) -> RequestCondition {
    RequestCondition {
        type_: type_.into(),
        status: status.into(),
        reason: reason.into(),
        message: "test condition".into(),
    }
}

#[test]
fn configured_lifetime_outlasts_agent_renewal() {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    scenarios!(run = |max_ttl| Config { max_ttl, ..Config::default() }
        .validate()
        .map_err(|error| assert!(matches!(error, Error::Configuration(_)), "{error}"));
        "must include the renewal grace period" {
            MIN_CERTIFICATE_TTL - Duration::from_secs(1) => Fails,
            MIN_CERTIFICATE_TTL => Yields(()),
        }
        "fractional seconds are unsupported" {
            MIN_CERTIFICATE_TTL + Duration::from_millis(1) => Fails,
        }
    );
}

#[test]
fn certificate_request_argument_contract() {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    let below_minimum =
        humantime::format_duration(MIN_CERTIFICATE_TTL - Duration::from_secs(1)).to_string();
    let minimum = humantime::format_duration(MIN_CERTIFICATE_TTL).to_string();
    let fractional =
        humantime::format_duration(MIN_CERTIFICATE_TTL + Duration::from_millis(1)).to_string();

    scenarios!(run = |(identifier, dns_names, ttl)| CertificateRequestMaterial::new(
        &spiffe_identity(), identifier, dns_names, ttl, default_max_ttl(),
    )
    .map(|request| request.lifetime)
    .map_err(|error| assert!(matches!(error, Error::InvalidArgument(_)), "{error}"));
        "invalid identifier" {
            ("invalid/id", None, None) => Fails,
        }
        "invalid DNS SAN" {
            (MACHINE_ID, Some("not a DNS name"), None) => Fails,
        }
        "malformed TTL" {
            (MACHINE_ID, None, Some("soon")) => Fails,
        }
        "must include the renewal grace period" {
            (MACHINE_ID, None, Some(below_minimum.as_str())) => Fails,
            (MACHINE_ID, None, Some(minimum.as_str())) => Yields(MIN_CERTIFICATE_TTL),
        }
        "fractional seconds are unsupported" {
            (MACHINE_ID, None, Some(fractional.as_str())) => Fails,
        }
    );
}

#[derive(Clone, Copy)]
enum Behavior {
    Issued,
    Denied,
    Pending,
    DeleteFailure,
    DeleteStall,
    PendingDeleteStall,
    CreateFailure,
}

#[derive(Default)]
struct Recorded {
    created: Option<CertificateRequest>,
    deleted: Vec<String>,
}

fn mock_client(ca: Arc<TestCa>, behavior: Behavior, recorded: Arc<Mutex<Recorded>>) -> Client {
    let service = service_fn(move |request: Request<kube::client::Body>| {
        let ca = ca.clone();
        let recorded = recorded.clone();
        async move {
            let (parts, body) = request.into_parts();
            let mut status_code = StatusCode::OK;
            let mut response = match parts.method {
                Method::POST => {
                    let body = body.collect().await.unwrap().to_bytes();
                    let mut resource: CertificateRequest = serde_json::from_slice(&body).unwrap();
                    resource.metadata.uid = Some("test-uid".into());
                    let expected =
                        "/apis/cert-manager.io/v1/namespaces/forge-system/certificaterequests";
                    assert_eq!(parts.uri.path(), expected);
                    recorded.lock().unwrap().created = Some(resource.clone());
                    if matches!(behavior, Behavior::CreateFailure) {
                        status_code = StatusCode::SERVICE_UNAVAILABLE;
                        json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"ServiceUnavailable", "message":"response lost after creation", "code":503})
                    } else {
                        serde_json::to_value(resource).unwrap()
                    }
                }
                Method::GET => {
                    let mut resource = recorded.lock().unwrap().created.clone().unwrap();
                    resource.status = Some(match behavior {
                        Behavior::Issued | Behavior::DeleteFailure | Behavior::DeleteStall => {
                            let seconds = resource
                                .spec
                                .duration
                                .trim_end_matches('s')
                                .parse()
                                .unwrap();
                            ca.sign(&resource.spec.request.0, Duration::from_secs(seconds))
                        }
                        Behavior::Denied => RequestStatus {
                            conditions: vec![condition("Denied", "True", "Policy")],
                            ..Default::default()
                        },
                        Behavior::Pending | Behavior::PendingDeleteStall => {
                            RequestStatus::default()
                        }
                        Behavior::CreateFailure => panic!("failed creation must not poll"),
                    });
                    serde_json::to_value(resource).unwrap()
                }
                Method::DELETE => {
                    recorded
                        .lock()
                        .unwrap()
                        .deleted
                        .push(parts.uri.path().into());
                    let body: Value =
                        serde_json::from_slice(&body.collect().await.unwrap().to_bytes()).unwrap();
                    if matches!(behavior, Behavior::CreateFailure) {
                        assert!(body.get("preconditions").is_none());
                    } else {
                        assert_eq!(body["preconditions"]["uid"], "test-uid");
                    }
                    if matches!(
                        behavior,
                        Behavior::DeleteStall | Behavior::PendingDeleteStall
                    ) {
                        std::future::pending::<()>().await;
                    }
                    if matches!(behavior, Behavior::DeleteFailure) {
                        status_code = StatusCode::FORBIDDEN;
                        json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"Forbidden", "message":"forbidden", "code":403})
                    } else {
                        json!({"apiVersion":"v1", "kind":"Status", "status":"Success"})
                    }
                }
                method => panic!("unexpected Kubernetes method {method}"),
            };
            // cert-manager responses may omit the optional false isCA field.
            if let Some(spec) = response.get_mut("spec").and_then(Value::as_object_mut) {
                assert_eq!(spec.remove("isCA"), Some(Value::Bool(false)));
            }
            Ok::<_, std::convert::Infallible>(
                Response::builder()
                    .status(status_code)
                    .body(Full::new(serde_json::to_vec(&response).unwrap().into()))
                    .unwrap(),
            )
        }
    });
    Client::new(service, "forge-system")
}

/// Exercises the provider through the Kubernetes wire contract and the public trait.
#[tokio::test]
async fn issuance_and_cleanup_contract() {
    let ca = Arc::new(TestCa::new());
    let trust = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(trust.path(), &ca.pem).unwrap();
    for (name, behavior, succeeds) in [
        ("issued", Behavior::Issued, true),
        ("denied", Behavior::Denied, false),
        ("timeout", Behavior::Pending, false),
        (
            "ambiguous creation failure still attempts cleanup",
            Behavior::CreateFailure,
            false,
        ),
        (
            "cleanup failure preserves issued credential",
            Behavior::DeleteFailure,
            true,
        ),
        (
            "stalled cleanup preserves issued credential",
            Behavior::DeleteStall,
            true,
        ),
        (
            "issuance timeout still attempts bounded cleanup",
            Behavior::PendingDeleteStall,
            false,
        ),
    ] {
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let provider = CertManagerCertificateProvider::new(
            mock_client(ca.clone(), behavior, recorded.clone()),
            Config {
                namespace: "forge-system".into(),
                issuer_name: "site-issuer".into(),
                issuer_kind: IssuerKind::ClusterIssuer,
                request_timeout_secs: 1,
                max_ttl: default_max_ttl(),
            },
            spiffe_identity(),
            trust.path().into(),
        )
        .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            provider.get_certificate(MACHINE_ID, None, None),
        )
        .await
        .unwrap_or_else(|_| panic!("{name}: issuance and cleanup must both be bounded"));
        assert_eq!(result.is_ok(), succeeds, "{name}: {result:?}");
        let recorded = recorded.lock().unwrap();
        let created = recorded.created.as_ref().unwrap();
        assert!(
            !created.spec.is_ca,
            "{name}: requests must be leaf certificates"
        );
        assert_eq!(
            created.metadata.labels.as_ref().unwrap()[MACHINE_ID_LABEL],
            MACHINE_ID
        );
        let request_uuid = created
            .metadata
            .name
            .as_ref()
            .unwrap()
            .strip_prefix("nico-machine-")
            .unwrap();
        assert!(
            Uuid::parse_str(request_uuid).is_ok(),
            "{name}: UUID-only request name"
        );
        assert_eq!(recorded.deleted.len(), 1, "{name}: request must be deleted");
        let csr = CertificateSigningRequestParams::from_pem(
            std::str::from_utf8(&created.spec.request.0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            csr.params.subject_alt_names,
            vec![rcgen::SanType::URI(SPIFFE.try_into().unwrap())]
        );
        assert_eq!(created.spec.issuer_ref.name, "site-issuer");
        assert!(
            std::str::from_utf8(&created.spec.request.0)
                .unwrap()
                .starts_with("-----BEGIN CERTIFICATE REQUEST-----")
        );
        if let Ok(certificate) = result {
            let key =
                KeyPair::from_pem(std::str::from_utf8(&certificate.private_key).unwrap()).unwrap();
            assert_eq!(key.algorithm(), &rcgen::PKCS_ECDSA_P256_SHA256);
            assert_eq!(certificate.issuing_ca, ca.pem.as_bytes());
        }
    }
}

#[test]
fn optional_issuer_chain_contract() {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    let ca = TestCa::new();
    let other_ca = TestCa::new();
    let request = CertificateRequestMaterial::new(
        &spiffe_identity(),
        MACHINE_ID,
        None,
        None,
        default_max_ttl(),
    )
    .unwrap();
    let mut status = ca.sign(&request.csr_pem().unwrap(), request.lifetime);
    status.ca = None;
    let validate = |trust: &[u8]| {
        request
            .validate(&status, trust)
            .map(|certificate| certificate.issuing_ca)
            .map_err(drop)
    };
    scenarios!(validate:
        "missing issuer chain accepts a directly trusted leaf" {
            ca.pem.as_bytes() => Yields(Vec::<u8>::new()),
        }
        "missing issuer chain still requires configured trust" {
            other_ca.pem.as_bytes() => Fails,
        }
    );
}

#[test]
fn rejects_incompatible_issued_material() {
    let ca = TestCa::new();
    let request = CertificateRequestMaterial::new(
        &spiffe_identity(),
        MACHINE_ID,
        None,
        None,
        default_max_ttl(),
    )
    .unwrap();
    let valid = ca.sign(&request.csr_pem().unwrap(), request.lifetime);
    assert!(request.validate(&valid, ca.pem.as_bytes()).is_ok());
    let other_key = CertificateRequestMaterial::new(
        &spiffe_identity(),
        MACHINE_ID,
        None,
        None,
        default_max_ttl(),
    )
    .unwrap();
    let wrong_key = ca.sign(&other_key.csr_pem().unwrap(), other_key.lifetime);
    let other_ca = TestCa::new();
    let mut wrong_identity_csr = CertificateSigningRequestParams::from_pem(
        std::str::from_utf8(&request.csr_pem().unwrap()).unwrap(),
    )
    .unwrap();
    wrong_identity_csr.params.subject_alt_names = vec![rcgen::SanType::URI(
        "spiffe://nico.local/forge-system/machine/other"
            .try_into()
            .unwrap(),
    )];
    wrong_identity_csr.params.not_after = OffsetDateTime::now_utc() + time::Duration::hours(432);
    let mut wrong_identity = valid.clone();
    wrong_identity.certificate = Some(ByteString(
        wrong_identity_csr
            .signed_by(&ca.issuer)
            .unwrap()
            .pem()
            .into_bytes(),
    ));
    let mut missing_usage_csr = CertificateSigningRequestParams::from_pem(
        std::str::from_utf8(&request.csr_pem().unwrap()).unwrap(),
    )
    .unwrap();
    missing_usage_csr.params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    missing_usage_csr.params.not_after = OffsetDateTime::now_utc() + time::Duration::hours(432);
    let mut missing_usage = valid.clone();
    missing_usage.certificate = Some(ByteString(
        missing_usage_csr
            .signed_by(&ca.issuer)
            .unwrap()
            .pem()
            .into_bytes(),
    ));
    let too_long = ca.sign(
        &request.csr_pem().unwrap(),
        request.lifetime + Duration::from_secs(3600),
    );
    let too_short = ca.sign(
        &request.csr_pem().unwrap(),
        MIN_CERTIFICATE_TTL - Duration::from_secs(1),
    );
    for (name, certificate, trust) in [
        ("different key", &wrong_key, ca.pem.as_bytes()),
        ("different identity", &wrong_identity, ca.pem.as_bytes()),
        ("missing usages", &missing_usage, ca.pem.as_bytes()),
        ("different CA", &valid, other_ca.pem.as_bytes()),
        ("extended lifetime", &too_long, ca.pem.as_bytes()),
        (
            "expires before the renewal interval ends",
            &too_short,
            ca.pem.as_bytes(),
        ),
    ] {
        assert!(request.validate(certificate, trust).is_err(), "{name}");
    }
}

/// UFM's manual-installation request preserves DNS names and caps its explicit TTL.
#[tokio::test]
async fn ufm_issuance_contract() {
    let ca = Arc::new(TestCa::new());
    let trust = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(trust.path(), &ca.pem).unwrap();
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let provider = CertManagerCertificateProvider::new(
        mock_client(ca.clone(), Behavior::Issued, recorded.clone()),
        Config {
            namespace: "forge-system".into(),
            issuer_name: "site-issuer".into(),
            issuer_kind: IssuerKind::ClusterIssuer,
            request_timeout_secs: 1,
            max_ttl: default_max_ttl(),
        },
        spiffe_identity(),
        trust.path().into(),
    )
    .unwrap();
    let certificate = provider
        .get_certificate(
            "fabric-a",
            Some("fabric-a.ufm.forge, fabric-a.ufm.example.com".into()),
            Some("365d".into()),
        )
        .await
        .unwrap();
    assert!(!certificate.private_key.is_empty());
    let recorded = recorded.lock().unwrap();
    let created = recorded.created.as_ref().unwrap();
    assert_eq!(
        created.metadata.labels.as_ref().unwrap()[FABRIC_LABEL],
        "fabric-a"
    );
    assert!(
        created
            .metadata
            .name
            .as_ref()
            .unwrap()
            .starts_with("nico-ufm-")
    );
    assert_eq!(created.spec.duration, "2592000s");
    let csr = CertificateSigningRequestParams::from_pem(
        std::str::from_utf8(&created.spec.request.0).unwrap(),
    )
    .unwrap();
    assert_eq!(
        csr.params.subject_alt_names,
        vec![
            rcgen::SanType::URI(
                "spiffe://nico.local/forge-system/machine/fabric-a"
                    .try_into()
                    .unwrap()
            ),
            rcgen::SanType::DnsName("fabric-a.ufm.forge".try_into().unwrap()),
            rcgen::SanType::DnsName("fabric-a.ufm.example.com".try_into().unwrap()),
        ]
    );
    assert_eq!(recorded.deleted.len(), 1);
}
