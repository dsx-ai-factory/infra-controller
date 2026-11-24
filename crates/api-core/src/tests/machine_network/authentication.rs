/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use carbide_authn::config::TrustConfig;
use p256::pkcs8::{DecodePrivateKey, LineEnding};
use rpc::forge::ManagedHostNetworkConfigRequest;
use rpc::forge::forge_client::ForgeClient;
use rpc::forge::forge_server::Forge;
use rpc::node_jwt::NodeJwtMinter;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Request};

use crate::cfg::file::AuthConfig;
use crate::listener::{ApiListenMode, ApiTlsConfig};
use crate::node_auth::NodeJwtValidator;
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures;

/// Exercise credential parsing and both authorization layers before the real handler.
#[test]
fn test_managed_host_network_config_authenticated_callers() {
    let args = sqlx::testing::TestArgs::new(concat!(
        module_path!(),
        "::test_managed_host_network_config_authenticated_callers"
    ));
    let test_fn: fn(sqlx::PgPool) -> _ = authenticated_callers;
    // Match Core's worker stack: the generated gRPC dispatcher exceeds libtest's default.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || sqlx_testing::TestFn::run_test(test_fn, args))
        .unwrap()
        .join()
        .expect("authenticated network-config test panicked");
}

async fn authenticated_callers(pool: sqlx::PgPool) {
    let env = api_fixtures::create_test_env(pool).await;
    let host = api_fixtures::create_managed_host_multi_dpu(&env, 2).await;
    let dpu_a = host.dpu().id;
    let dpu_b = host.dpu_n(1).id;

    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, contents: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path.to_string_lossy().into_owned()
    };
    let mut ca_params = rcgen::CertificateParams::default();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::PrintableString("network-config-test-ca".try_into().unwrap()),
    );
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_pem = ca_params.clone().self_signed(&ca_key).unwrap().pem();
    let ca_path = write("ca.pem", &ca_pem);
    let issuer = rcgen::Issuer::new(ca_params, ca_key);

    let server_key = rcgen::KeyPair::generate().unwrap();
    let server_cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .signed_by(&server_key, &issuer)
        .unwrap();
    let server_key = p256::SecretKey::from_pkcs8_pem(&server_key.serialize_pem())
        .unwrap()
        .to_sec1_pem(LineEnding::LF)
        .unwrap();
    let tls = ApiTlsConfig {
        identity_pemfile_path: write("server.pem", &server_cert.pem()),
        identity_keyfile_path: write("server.key", server_key.as_str()),
        root_cafile_path: ca_path.clone(),
        admin_root_cafile_path: ca_path.clone(),
    };

    let issue_client = |spiffe_path: Option<&str>| {
        let mut params = rcgen::CertificateParams::default();
        params.use_authority_key_identifier_extension = true;
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        if let Some(path) = spiffe_path {
            params.subject_alt_names = vec![rcgen::SanType::URI(
                format!("spiffe://network-config.test{path}")
                    .try_into()
                    .unwrap(),
            )];
        } else {
            params.distinguished_name.push(
                rcgen::DnType::OrganizationalUnitName,
                rcgen::DnValue::PrintableString("nico-cli-client".try_into().unwrap()),
            );
        }
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.signed_by(&key, &issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    };
    let (machine_cert, machine_key) = issue_client(Some(&format!("/machine/{dpu_a}")));
    let machine_identity = Identity::from_pem(&machine_cert, &machine_key);
    let token = NodeJwtMinter::new(
        write("machine.pem", &machine_cert),
        write("machine.key", &machine_key),
    )
    .current()
    .expect("mint a node JWT from the DPU certificate");
    let admin = issue_client(None);
    let site_agent = issue_client(Some("/service/elektra-site-agent"));
    let machineatron = issue_client(Some("/service/machine-a-tron"));
    let unrelated_service = issue_client(Some("/service/unrelated-service"));

    let mut config = (*env.config).clone();
    config.bypass_rbac = false;
    config.node_auth.enabled = true;
    config.node_auth.mtls_enabled = true;
    config.auth = Some(AuthConfig {
        permissive_mode: false,
        allow_machineatron_scout_stream: false,
        casbin_policy_file: Some(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../api/casbin-policy.csv"),
        ),
        cli_certs: None,
        trust: Some(TrustConfig {
            spiffe_trust_domain: "network-config.test".to_string(),
            spiffe_service_base_paths: vec!["/service/".to_string()],
            spiffe_machine_base_path: "/machine/".to_string(),
            additional_issuer_cns: vec!["network-config-test-ca".to_string()],
        }),
    });
    let config = Arc::new(config);
    let mut api = TestApiBuilder::new(
        env.pool.clone(),
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_runtime_config(config.clone())
    .with_credential_manager(env.api.credential_manager.clone())
    .with_eth_data(env.api.eth_data.clone())
    .build();
    api.node_jwt_validator = Some(Arc::new(
        NodeJwtValidator::from_root_ca_file(&ca_path, &config.node_auth).unwrap(),
    ));
    let api = Arc::new(api);
    let cancel = CancellationToken::new();
    let _cancel_guard = cancel.clone().drop_guard();
    let mut tasks = JoinSet::new();
    let firmware_artifact_cache = carbide_firmware::FirmwareArtifactCache::new(
        dir.path().join("firmware-artifacts"),
        4 * 1024 * 1024 * 1024,
        carbide_firmware::FirmwareDownloader::new(
            carbide_instrument::Instrumentation::from_meter_provider(
                &opentelemetry::metrics::noop::NoopMeterProvider::new(),
            ),
        ),
        cancel.clone(),
        &mut tasks,
    )
    .unwrap();
    let address = crate::listener::start(
        &mut tasks,
        api.clone(),
        ApiListenMode::Tls(Arc::new(tls)),
        "127.0.0.1:0".parse().unwrap(),
        &config.auth,
        env.test_meter.meter(),
        None,
        firmware_artifact_cache,
        cancel.clone(),
    )
    .await
    .unwrap();

    for (scenario, identity, bearer, requested_dpu, expected_code) in [
        (
            "matching certificate",
            Some(machine_identity.clone()),
            None,
            dpu_a,
            Code::Ok,
        ),
        (
            "mismatched certificate",
            Some(machine_identity),
            None,
            dpu_b,
            Code::PermissionDenied,
        ),
        (
            "matching JWT without client certificate",
            None,
            Some(token.as_str()),
            dpu_a,
            Code::Ok,
        ),
        (
            "mismatched JWT without client certificate",
            None,
            Some(token.as_str()),
            dpu_b,
            Code::PermissionDenied,
        ),
        (
            "administrative certificate",
            Some(Identity::from_pem(admin.0, admin.1)),
            None,
            dpu_b,
            Code::Ok,
        ),
        (
            "SiteAgent certificate",
            Some(Identity::from_pem(site_agent.0, site_agent.1)),
            None,
            dpu_b,
            Code::Ok,
        ),
        (
            "Machineatron certificate",
            Some(Identity::from_pem(machineatron.0, machineatron.1)),
            None,
            dpu_b,
            Code::Ok,
        ),
        // A trusted but unauthorized service must not reach the identity-free handler path.
        (
            "unauthorized service certificate",
            Some(Identity::from_pem(unrelated_service.0, unrelated_service.1)),
            None,
            dpu_b,
            Code::PermissionDenied,
        ),
    ] {
        let mut client_tls = ClientTlsConfig::new()
            .domain_name("localhost")
            .ca_certificate(Certificate::from_pem(&ca_pem));
        if let Some(identity) = identity {
            client_tls = client_tls.identity(identity);
        }
        let channel = Endpoint::from_shared(format!("https://{address}"))
            .unwrap()
            .tls_config(client_tls)
            .unwrap()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(5))
            .connect()
            .await
            .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        let mut client = ForgeClient::new(channel);
        let message = ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(requested_dpu),
        };
        let mut request = Request::new(message.clone());
        if let Some(token) = bearer {
            request
                .metadata_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
        }
        match client.get_managed_host_network_config(request).await {
            Ok(response) => {
                assert_eq!(expected_code, Code::Ok, "{scenario}");
                let expected = api
                    .get_managed_host_network_config(Request::new(message))
                    .await
                    .unwrap()
                    .into_inner();
                assert_eq!(response.into_inner(), expected, "{scenario}");
            }
            Err(status) => assert_eq!(status.code(), expected_code, "{scenario}: {status}"),
        }
    }

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), tasks.join_all())
        .await
        .expect("listener shuts down");
}
