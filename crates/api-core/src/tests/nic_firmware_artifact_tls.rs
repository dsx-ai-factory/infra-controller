/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Qualify the artifact route through Core's shipping TLS and identity layers.

use std::sync::Arc;
use std::time::Duration;

use carbide_authn::config::TrustConfig;
use carbide_uuid::machine::MachineType;
use model::machine::ManagedHostState;
use p256::pkcs8::{DecodePrivateKey, LineEnding};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use reqwest::{Client, Identity, StatusCode, Version};
use tokio::task::JoinSet;

use super::{IMAGE, Origin, admitted, cache, create, digest, machine_id, seed, uri};
use crate::cfg::file::AuthConfig;
use crate::listener::{self, ApiListenMode, ApiTlsConfig};

#[cfg(target_os = "linux")]
#[path = "nic_firmware_artifact_tls_supervisor.rs"]
mod supervisor;

// The shipping TLS listener must use the exact verified machine identity.
// Casbin is not configured here; artifact authorization is enforced by the route.
#[crate::sqlx_test]
async fn artifact_route_uses_tls_machine_identity(pool: sqlx::PgPool) {
    let environment = crate::tests::create_test_env(pool.clone()).await;
    assert!(environment.api.runtime_config.node_auth.mtls_enabled);
    assert!(!environment.api.runtime_config.bypass_rbac);
    let root = tempfile::tempdir().expect("TLS fixture directory");
    let host = machine_id(97, MachineType::Host);
    let foreign_host = machine_id(98, MachineType::Host);
    let origin = Origin::start(false).await;
    let operation = admitted(&origin);
    create(
        &pool,
        host,
        ManagedHostState::NicFirmwareUpdating {
            operation: operation.clone(),
        },
    )
    .await;
    create(&pool, foreign_host, ManagedHostState::Ready).await;

    // Fresh fixtures avoid depending on checked-in certificate dates.
    let ca_key = KeyPair::generate().expect("CA key");
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = ca_params.self_signed(&ca_key).expect("CA certificate");
    let issuer = Issuer::from_params(&ca_params, &ca_key);
    let server_key = KeyPair::generate().expect("server key");
    let mut server_params = CertificateParams::new(vec!["localhost".into()]).expect("server SAN");
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server = server_params
        .signed_by(&server_key, &issuer)
        .expect("server certificate");
    let client_key = KeyPair::generate().expect("client key");
    let client_certificate = |host| {
        let mut params = CertificateParams::default();
        params.subject_alt_names = vec![SanType::URI(
            format!("spiffe://artifact.test/machine/{host}")
                .try_into()
                .expect("machine URI"),
        )];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params
            .signed_by(&client_key, &issuer)
            .expect("client certificate")
    };
    let client = client_certificate(host);
    let foreign = client_certificate(foreign_host);
    let server_sec1 = p256::SecretKey::from_pkcs8_pem(&server_key.serialize_pem())
        .expect("server PKCS8")
        .to_sec1_pem(LineEnding::LF)
        .expect("server SEC1");
    let write = |name: &str, contents: &str| {
        let path = root.path().join(name);
        std::fs::write(&path, contents).expect("write TLS fixture");
        path.to_string_lossy().into_owned()
    };
    let tls = Arc::new(ApiTlsConfig {
        identity_pemfile_path: write("server.pem", &server.pem()),
        identity_keyfile_path: write("server-key.pem", &server_sec1),
        root_cafile_path: write("ca.pem", &ca.pem()),
        admin_root_cafile_path: root
            .path()
            .join("no-admin-ca.pem")
            .to_string_lossy()
            .into_owned(),
    });
    let auth = Some(AuthConfig {
        permissive_mode: true,
        allow_machineatron_scout_stream: false,
        casbin_policy_file: None,
        cli_certs: None,
        trust: Some(TrustConfig {
            spiffe_trust_domain: "artifact.test".into(),
            spiffe_service_base_paths: vec!["/service/".into()],
            spiffe_machine_base_path: "/machine/".into(),
            additional_issuer_cns: vec![],
        }),
    });
    let cache = cache(256);
    let cancel = cache.cancel.clone();
    let mut tasks = JoinSet::new();
    let address = listener::start(
        &mut tasks,
        environment.api,
        ApiListenMode::Tls(tls),
        "127.0.0.1:0".parse().unwrap(),
        &auth,
        opentelemetry::global::meter("artifact_tls_test"),
        None,
        cache.cache.clone(),
        cancel.clone(),
    )
    .await
    .expect("start TLS listener");
    let url = format!(
        "https://localhost:{}{}",
        address.port(),
        uri(&operation, &digest(IMAGE), "image")
    );
    let client_builder = || {
        Client::builder()
            .no_proxy()
            .add_root_certificate(
                reqwest::Certificate::from_pem(ca.pem().as_bytes()).expect("trusted CA"),
            )
            .timeout(Duration::from_secs(5))
    };
    let anonymous = client_builder().build().expect("anonymous client");
    let authenticated_client = |certificate: &rcgen::Certificate| {
        client_builder()
            .identity(
                Identity::from_pem(
                    format!("{}{}", certificate.pem(), client_key.serialize_pem()).as_bytes(),
                )
                .expect("client identity"),
            )
            .build()
            .expect("authenticated client")
    };
    let authenticated = authenticated_client(&client);
    let foreign = authenticated_client(&foreign);

    // Owned work exists. Neither an anonymous connection nor another verified
    // host may create a namespace or reach its source.
    for (client, expected) in [
        (&anonymous, StatusCode::UNAUTHORIZED),
        (&foreign, StatusCode::NOT_FOUND),
    ] {
        let response = client.get(&url).send().await.expect("artifact response");
        assert_eq!(response.version(), Version::HTTP_2);
        assert_eq!(response.status(), expected);
        response.bytes().await.expect("consume response");
    }
    assert!(!cache.namespace().exists());
    assert!(origin.paths().is_empty());
    let response = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = authenticated
                .get(&url)
                .header("x-artifact-request-secret", "do-not-forward")
                .send()
                .await
                .expect("owned artifact response");
            assert_eq!(response.version(), Version::HTTP_2);
            if response.status() != StatusCode::ACCEPTED {
                break response;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned artifact publication");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.expect("TLS artifact bytes"), IMAGE);
    assert_eq!(
        std::fs::read(cache.namespace().join(digest(IMAGE))).expect("published cache file"),
        IMAGE
    );

    assert_eq!(origin.paths(), ["/image"]);

    #[cfg(target_os = "linux")]
    {
        use libmlx::firmware::core_download::CoreFirmwareDownloadConfig;

        let client_sec1 = p256::SecretKey::from_pkcs8_pem(&client_key.serialize_pem())
            .expect("client PKCS8")
            .to_sec1_pem(LineEnding::LF)
            .expect("client SEC1");
        supervisor::supervisor_stages_from_core(
            &pool,
            host,
            &operation,
            &origin,
            &cache.namespace(),
            CoreFirmwareDownloadConfig {
                core_url: format!("https://localhost:{}", address.port()),
                root_ca_path: root.path().join("ca.pem").to_string_lossy().into_owned(),
                client_cert_path: write("client.pem", &client.pem()),
                client_key_path: write("client-key.pem", &client_sec1),
                enforce_tls: true,
                require_client_cert: true,
                use_mgmt_vrf: false,
                request_timeout: Some(Duration::from_secs(1)),
                socks_proxy: None,
                connect_retries_max: Some(0),
                connect_retries_interval: None,
            },
        )
        .await;
    }

    #[cfg(target_os = "linux")]
    let expected_source_paths = ["/image", "/image", "/configuration"];
    #[cfg(not(target_os = "linux"))]
    let expected_source_paths = ["/image"];

    // Cached bytes confer no authority. Check both identities again, then
    // retire the owner's operation and require its previously valid URL to fail.
    for (client, expected) in [
        (&anonymous, StatusCode::UNAUTHORIZED),
        (&foreign, StatusCode::NOT_FOUND),
    ] {
        let response = client
            .get(&url)
            .send()
            .await
            .expect("warm artifact response");
        assert_eq!(response.status(), expected);
        response.bytes().await.expect("consume warm denial");
    }
    seed(&pool, host.try_into().expect("host ID"), None).await;
    let stale = authenticated
        .get(&url)
        .send()
        .await
        .expect("retired operation response");
    assert_eq!(stale.status(), StatusCode::NOT_FOUND);
    stale
        .bytes()
        .await
        .expect("consume retired operation denial");
    assert_eq!(origin.paths(), expected_source_paths);
    drop(anonymous);
    drop(authenticated);
    drop(foreign);
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(result) = tasks.join_next().await {
            result.expect("listener task");
        }
    })
    .await
    .expect("TLS listener shutdown");
    cache.finish().await;
    origin.finish().await;
}
