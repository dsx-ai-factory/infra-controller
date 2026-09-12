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

//! Probes against nico-api's gRPC surface, built on the `carbide-rpc` crate's
//! TLS client and generated types. Adding a nico-api probe means adding to
//! this module — nothing in the framework or the REST module changes.
//!
//! TODO(#5360-followup): the site-stats probe family (host-ingestion progress,
//! firmware-update tracking) belongs here as further read-only gRPC probes.

use std::time::{Duration, Instant};

use eyre::eyre;
use forge_tls::client_config::ClientCert;
use rpc::forge::{MachineSearchConfig, MachinesByIdsRequest};
use rpc::forge_tls_client::{ForgeClientConfig, ForgeTlsClient};

use crate::config;
use crate::framework::{ObservationRecorder, Probe};

/// Probes nico-api with the two read-only calls behind `machine show`:
/// FindMachineIds (the lightest DB-touching read) and FindMachinesByIds on the
/// first page (the full read path: handler + machine and interface joins).
/// Both operations are observed separately.
pub(crate) struct MachinesProbe {
    cfg: config::GrpcProbe,
}

impl MachinesProbe {
    pub(crate) fn new(cfg: config::GrpcProbe) -> Self {
        Self { cfg }
    }
}

#[async_trait::async_trait]
impl Probe for MachinesProbe {
    fn name(&self) -> &'static str {
        "grpc_machines"
    }

    fn api(&self) -> &'static str {
        "nico-api"
    }

    fn interval(&self) -> Duration {
        self.cfg.interval
    }

    fn timeout(&self) -> Duration {
        self.cfg.timeout
    }

    /// Each run builds a fresh client, so find_machine_ids includes TCP + TLS
    /// + HTTP/2 setup on top of the RPC itself. That is deliberate for a
    /// canary: it measures the cold path a new client experiences (and
    /// re-reads the certs, making rotation need no restart) rather than the
    /// warm path of a pooled connection. For the same reason this uses
    /// `ForgeTlsClient::build` (one attempt, like agent/main_loop and
    /// host-support/registration) rather than `retry_build` or the pooled
    /// `ForgeApiClient` wrapper — their connect retries and per-connect
    /// version() health check would hide exactly the failures a canary
    /// exists to count.
    async fn run(&self, recorder: &ObservationRecorder) -> eyre::Result<()> {
        // The rpc client treats an unreadable-but-present CA bundle as an
        // empty root store, which would only surface as a confusing handshake
        // failure; validate it up front so a bad mount names itself.
        preflight_ca(&self.cfg.tls.ca).await?;

        let mut client_config = ForgeClientConfig::new(
            self.cfg.tls.ca.clone(),
            Some(ClientCert {
                cert_path: self.cfg.tls.cert.clone(),
                key_path: self.cfg.tls.key.clone(),
            }),
        );
        // Server verification is always on — there is deliberately no insecure
        // mode. (The constructor honors DISABLE_TLS_ENFORCEMENT for local
        // development of other components; a canary measuring the real TLS
        // path must not.)
        client_config.enforce_tls = true;

        let url = format!("https://{}", self.cfg.target);
        let mut client = ForgeTlsClient::new(&client_config)
            .build(&url)
            .await
            .map_err(|e| eyre!("dial: {e}"))?;

        let start = Instant::now();
        let ids = client
            .find_machine_ids(MachineSearchConfig::default())
            .await
            .map_err(|e| eyre!("FindMachineIds: {e}"))?;
        recorder.record("find_machine_ids", start.elapsed());

        let mut page = ids.into_inner().machine_ids;
        if page.is_empty() {
            // An empty site is a healthy answer — the API and its DB read path
            // responded; there is just nothing to fetch details for.
            return Ok(());
        }
        page.truncate(self.cfg.effective_page_size());

        let start = Instant::now();
        client
            .find_machines_by_ids(MachinesByIdsRequest {
                machine_ids: page,
                include_history: false,
            })
            .await
            .map_err(|e| eyre!("FindMachinesByIds: {e}"))?;
        recorder.record("find_machines_by_ids", start.elapsed());
        Ok(())
    }
}

/// Fails with the offending path when the CA bundle is missing or contains no
/// parseable certificate.
async fn preflight_ca(path: &str) -> eyre::Result<()> {
    let pem = tokio::fs::read(path)
        .await
        .map_err(|e| eyre!("read CA: {e}"))?;
    let mut cursor = std::io::Cursor::new(&pem[..]);
    let parsed = rustls_pemfile::certs(&mut cursor).flatten().count();
    if parsed == 0 {
        return Err(eyre!("no certificates parsed from {path}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose,
    };
    use tonic::transport::server::TcpIncoming;
    use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};

    use super::*;
    use crate::config::TlsConfig;

    /// The stub Forge server, generated from `forge.proto` filtered down to
    /// the two probed RPCs (see build.rs). Wire-compatible with the rpc
    /// crate's client types without sharing Rust types. Generated code is not
    /// held to the workspace's lint bar.
    #[allow(
        dead_code,
        unused_imports,
        unreachable_pub,
        clippy::all,
        clippy::pedantic
    )]
    mod stubpb {
        pub mod common {
            include!(concat!(env!("OUT_DIR"), "/common.rs"));
        }
        pub mod dns {
            include!(concat!(env!("OUT_DIR"), "/dns.rs"));
        }
        pub mod forge {
            include!(concat!(env!("OUT_DIR"), "/forge.rs"));
        }
        pub mod health {
            include!(concat!(env!("OUT_DIR"), "/health.rs"));
        }
        pub mod machine_discovery {
            include!(concat!(env!("OUT_DIR"), "/machine_discovery.rs"));
        }
        pub mod measured_boot {
            include!(concat!(env!("OUT_DIR"), "/measured_boot.rs"));
        }
        pub mod mlx_device {
            include!(concat!(env!("OUT_DIR"), "/mlx_device.rs"));
        }
        pub mod scout_firmware_upgrade {
            include!(concat!(env!("OUT_DIR"), "/scout_firmware_upgrade.rs"));
        }
        pub mod site_explorer {
            include!(concat!(env!("OUT_DIR"), "/site_explorer.rs"));
        }
    }

    use stubpb::forge::forge_server::{Forge, ForgeServer};

    /// Valid machine ids, taken from carbide_uuid's own examples — the rpc
    /// crate's client decodes ids through `carbide_uuid`, which validates the
    /// full format (prefix, type/source chars, and the base32 hardware id
    /// including its padding bits), so arbitrary strings are rejected.
    const ID_1: &str = "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";
    const ID_2: &str = "fm100hsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g";
    const ID_3: &str = "fm100dtjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";

    #[derive(Default)]
    struct StubState {
        machine_ids: Vec<&'static str>,
        fail_find_ids: AtomicBool,
        fail_find_by_ids: AtomicBool,
        by_ids_requests: Mutex<Vec<stubpb::forge::MachinesByIdsRequest>>,
    }

    struct StubForge {
        state: Arc<StubState>,
    }

    #[tonic::async_trait]
    impl Forge for StubForge {
        async fn find_machine_ids(
            &self,
            _request: tonic::Request<stubpb::forge::MachineSearchConfig>,
        ) -> Result<tonic::Response<stubpb::common::MachineIdList>, tonic::Status> {
            if self.state.fail_find_ids.load(Ordering::SeqCst) {
                return Err(tonic::Status::internal("stub: find_machine_ids failing"));
            }
            Ok(tonic::Response::new(stubpb::common::MachineIdList {
                machine_ids: self
                    .state
                    .machine_ids
                    .iter()
                    .map(|id| stubpb::common::MachineId { id: id.to_string() })
                    .collect(),
            }))
        }

        async fn find_machines_by_ids(
            &self,
            request: tonic::Request<stubpb::forge::MachinesByIdsRequest>,
        ) -> Result<tonic::Response<stubpb::forge::MachineList>, tonic::Status> {
            let request = request.into_inner();
            self.state
                .by_ids_requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(request);
            if self.state.fail_find_by_ids.load(Ordering::SeqCst) {
                return Err(tonic::Status::internal(
                    "stub: find_machines_by_ids failing",
                ));
            }
            Ok(tonic::Response::new(stubpb::forge::MachineList::default()))
        }
    }

    /// Freshly minted CA, server identity for 127.0.0.1, and a client keypair
    /// the server's client-CA verification accepts.
    struct TlsMaterial {
        ca: Issuer<'static, KeyPair>,
        ca_pem: String,
        server_cert_pem: String,
        server_key_pem: String,
        client_cert_pem: String,
        client_key_pem: String,
    }

    fn mint_material() -> TlsMaterial {
        let mut ca_params = CertificateParams::new(Vec::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "site-health-probe test ca");
        ca_params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        ca_params.key_usages.push(KeyUsagePurpose::KeyCertSign);
        let ca_key = KeyPair::generate().expect("ca key");
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca cert");
        let ca_pem = ca_cert.pem();
        let ca = Issuer::new(ca_params, ca_key);

        let (server_cert_pem, server_key_pem) = mint_leaf(&ca, ExtendedKeyUsagePurpose::ServerAuth);
        let (client_cert_pem, client_key_pem) = mint_leaf(&ca, ExtendedKeyUsagePurpose::ClientAuth);

        TlsMaterial {
            ca,
            ca_pem,
            server_cert_pem,
            server_key_pem,
            client_cert_pem,
            client_key_pem,
        }
    }

    fn mint_leaf(
        ca: &Issuer<'static, KeyPair>,
        purpose: ExtendedKeyUsagePurpose,
    ) -> (String, String) {
        let mut params =
            CertificateParams::new(vec!["127.0.0.1".to_string()]).expect("leaf params");
        params
            .distinguished_name
            .push(DnType::CommonName, "site-health-probe test leaf");
        params.extended_key_usages.push(purpose);
        let key = KeyPair::generate().expect("leaf key");
        let cert = params.signed_by(&key, ca).expect("leaf cert");
        (cert.pem(), key.serialize_pem())
    }

    /// Serves the stub over mTLS on 127.0.0.1 and returns its address; the
    /// server requires a client certificate signed by the test CA.
    async fn spawn_stub(material: &TlsMaterial, state: Arc<StubState>) -> SocketAddr {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .ok(); // another test may already have installed it

        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(
                &material.server_cert_pem,
                &material.server_key_pem,
            ))
            .client_ca_root(Certificate::from_pem(&material.ca_pem));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub listener");
        let addr = listener.local_addr().expect("stub addr");

        tokio::spawn(
            Server::builder()
                .tls_config(tls)
                .expect("stub tls config")
                .add_service(ForgeServer::new(StubForge { state }))
                .serve_with_incoming(TcpIncoming::from(listener)),
        );
        addr
    }

    /// A probe config whose TLS paths live in `dir`, populated from `material`.
    fn probe_config(
        dir: &tempfile::TempDir,
        addr: SocketAddr,
        material: &TlsMaterial,
    ) -> config::GrpcProbe {
        let path = |name: &str| {
            dir.path()
                .join(name)
                .to_str()
                .expect("utf-8 path")
                .to_string()
        };
        std::fs::write(path("ca.crt"), &material.ca_pem).expect("write ca");
        std::fs::write(path("tls.crt"), &material.client_cert_pem).expect("write cert");
        std::fs::write(path("tls.key"), &material.client_key_pem).expect("write key");
        config::GrpcProbe {
            enabled: true,
            target: addr.to_string(),
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(5),
            page_size: 2,
            tls: TlsConfig {
                ca: path("ca.crt"),
                cert: path("tls.crt"),
                key: path("tls.key"),
            },
        }
    }

    #[tokio::test]
    async fn both_operations_run_and_page_is_truncated() {
        let material = mint_material();
        let state = Arc::new(StubState {
            machine_ids: vec![ID_1, ID_2, ID_3],
            ..StubState::default()
        });
        let addr = spawn_stub(&material, Arc::clone(&state)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let probe = MachinesProbe::new(probe_config(&dir, addr, &material));

        let recorder = ObservationRecorder::default();
        probe.run(&recorder).await.expect("probe succeeds");

        let requests = state
            .by_ids_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .machine_ids
                .iter()
                .map(|id| id.id.as_str())
                .collect::<Vec<_>>(),
            vec![ID_1, ID_2],
            "the detail read is truncated to page_size"
        );
        assert!(!requests[0].include_history, "history is never requested");
    }

    #[tokio::test]
    async fn empty_site_is_healthy() {
        let material = mint_material();
        let state = Arc::new(StubState::default());
        let addr = spawn_stub(&material, Arc::clone(&state)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let probe = MachinesProbe::new(probe_config(&dir, addr, &material));

        let recorder = ObservationRecorder::default();
        probe
            .run(&recorder)
            .await
            .expect("an empty site is healthy");

        assert!(
            state
                .by_ids_requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty(),
            "no detail read happens without ids"
        );
    }

    #[tokio::test]
    async fn errors_propagate_with_partial_observations() {
        let material = mint_material();
        let state = Arc::new(StubState {
            machine_ids: vec![ID_1],
            ..StubState::default()
        });
        let addr = spawn_stub(&material, Arc::clone(&state)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let probe = MachinesProbe::new(probe_config(&dir, addr, &material));

        state.fail_find_ids.store(true, Ordering::SeqCst);
        let recorder = ObservationRecorder::default();
        let err = probe
            .run(&recorder)
            .await
            .expect_err("ids failure surfaces");
        assert!(
            err.to_string().contains("FindMachineIds"),
            "error names the failing operation: {err:#}"
        );
        assert!(
            recorder.take().is_empty(),
            "a failed first operation records nothing"
        );

        state.fail_find_ids.store(false, Ordering::SeqCst);
        state.fail_find_by_ids.store(true, Ordering::SeqCst);
        let err = probe
            .run(&recorder)
            .await
            .expect_err("byIds failure surfaces");
        assert!(
            err.to_string().contains("FindMachinesByIds"),
            "error names the failing operation: {err:#}"
        );
        assert_eq!(
            recorder
                .take()
                .iter()
                .map(|o| o.operation)
                .collect::<Vec<_>>(),
            vec!["find_machine_ids"],
            "the observation made before the failure is kept"
        );
    }

    /// The probe must reject a server whose certificate does not chain to the
    /// configured CA — enforce_tls is the guarantee the bearer of the SPIFFE
    /// identity relies on.
    #[tokio::test]
    async fn untrusted_server_certificate_is_rejected() {
        let material = mint_material();
        let state = Arc::new(StubState::default());
        let addr = spawn_stub(&material, Arc::clone(&state)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = probe_config(&dir, addr, &material);
        // Trust a different CA than the one that signed the server identity;
        // the client certificate stays valid so only server verification can
        // fail the run.
        let untrusted = mint_material();
        std::fs::write(&cfg.tls.ca, &untrusted.ca_pem).expect("swap trusted CA");

        let recorder = ObservationRecorder::default();
        let err = MachinesProbe::new(cfg)
            .run(&recorder)
            .await
            .expect_err("a server outside the trusted CA must be rejected");
        assert!(
            err.to_string().contains("FindMachineIds"),
            "the rejection surfaces on the first RPC: {err:#}"
        );
        assert!(recorder.take().is_empty(), "nothing is measured");
    }

    #[tokio::test]
    async fn certs_are_reloaded_on_every_run() {
        let material = mint_material();
        let state = Arc::new(StubState::default());
        let addr = spawn_stub(&material, Arc::clone(&state)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = probe_config(&dir, addr, &material);
        let probe = MachinesProbe::new(cfg.clone());
        let recorder = ObservationRecorder::default();

        probe.run(&recorder).await.expect("initial material works");

        // cert-manager rotation: a new keypair signed by the same CA must be
        // picked up without constructing a new probe.
        let (rotated_cert, rotated_key) =
            mint_leaf(&material.ca, ExtendedKeyUsagePurpose::ClientAuth);
        std::fs::write(&cfg.tls.cert, rotated_cert).expect("rotate cert");
        std::fs::write(&cfg.tls.key, rotated_key).expect("rotate key");
        probe.run(&recorder).await.expect("rotated material works");

        // A clobbered key proves the files really are re-read: the next run
        // must fail rather than keep using a cached identity.
        std::fs::write(&cfg.tls.key, "not a key").expect("clobber key");
        let _ = probe
            .run(&recorder)
            .await
            .expect_err("clobbered key fails the run");
    }

    #[tokio::test]
    async fn bad_ca_bundle_names_itself() {
        let material = mint_material();
        let state = Arc::new(StubState::default());
        let addr = spawn_stub(&material, Arc::clone(&state)).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cfg = probe_config(&dir, addr, &material);
        let recorder = ObservationRecorder::default();

        let missing = dir.path().join("absent.crt");
        cfg.tls.ca = missing.to_str().expect("utf-8 path").to_string();
        let err = MachinesProbe::new(cfg.clone())
            .run(&recorder)
            .await
            .expect_err("missing CA fails");
        assert!(err.to_string().contains("read CA"), "got: {err:#}");

        std::fs::write(&cfg.tls.ca, "garbage").expect("write garbage ca");
        let err = MachinesProbe::new(cfg)
            .run(&recorder)
            .await
            .expect_err("garbage CA fails");
        assert!(
            err.to_string().contains("no certificates parsed from"),
            "got: {err:#}"
        );
    }
}
