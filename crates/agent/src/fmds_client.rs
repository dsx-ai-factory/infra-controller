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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use carbide_host_support::agent_config::MachineIdentityConfig;
use eyre::{WrapErr, eyre};
use forge_dpu_fmds_shared::machine_identity::MachineIdentityParams;
use opentelemetry::metrics::Meter;
use rpc::fmds::fmds_config_service_client::FmdsConfigServiceClient;
use rpc::fmds::{
    FmdsConfigUpdate, FmdsMachineIdentityConfig, IbDevice, IbInstance, UpdateConfigRequest,
};
use rpc::forge::ManagedHostNetworkConfigResponse;
use tonic::transport::{Channel, Endpoint};

use crate::instance_metadata_endpoint::InstanceMetadataRouterStateImpl;
use crate::instrumentation::FmdsPush;
use crate::periodic_config_fetcher::InstanceMetadata;

/// FmdsUpdater abstracts over embedded vs external FMDS
/// updates so the main loop doesn't need to care which
/// mode it's in. It's all handled in here.
pub(super) enum FmdsUpdater {
    /// Embedded will update FMDS state directly within the
    /// carbide-dpu-agent (because the FMDS listener is in
    /// the agent).
    Embedded(Arc<InstanceMetadataRouterStateImpl>),
    /// External will send FMDS updates to an FMDS server,
    /// which is colocated on the same DPU, possibly in its
    /// own container.
    External {
        address: String,
        machine_identity: MachineIdentityConfig,
        connect_timeout: Duration,
        last_connect_succeeded: Arc<AtomicBool>,
    },
}

pub(super) fn register_external_connection_metric(meter: &Meter) -> Arc<AtomicBool> {
    let last_connect_succeeded = Arc::new(AtomicBool::new(false));
    let connection_status = Arc::clone(&last_connect_succeeded);
    meter
        .u64_observable_gauge("carbide_dpu_agent_fmds_external_connected")
        .with_description(
            "Result of the DPU agent's latest connection attempt to its configured external FMDS (1 for success, 0 before success or after failure)",
        )
        .with_callback(move |observer| {
            observer.observe(
                u64::from(connection_status.load(Ordering::Relaxed)),
                &[],
            )
        })
        .build();
    last_connect_succeeded
}

impl FmdsUpdater {
    pub(super) async fn update(
        &mut self,
        instance_data: Option<Arc<InstanceMetadata>>,
        network_config: Option<Arc<ManagedHostNetworkConfigResponse>>,
    ) {
        match self {
            FmdsUpdater::External {
                address,
                machine_identity,
                connect_timeout,
                last_connect_succeeded,
            } => {
                let connection =
                    FmdsGrpcClient::connect(address, machine_identity.clone(), *connect_timeout)
                        .await;
                last_connect_succeeded.store(connection.is_ok(), Ordering::Relaxed);

                let result = match connection {
                    Ok(mut client) => client.update_config(&instance_data, &network_config).await,
                    Err(err) => Err(err),
                };

                // A failed push is dropped: the next main-loop iteration
                // reconnects and pushes again.
                match result {
                    Ok(()) => FmdsPush::Succeeded.emit(),
                    Err(err) => FmdsPush::Failed {
                        error: format!("{err:#}"),
                        fmds_address: address.clone(),
                    }
                    .emit(),
                }
            }
            FmdsUpdater::Embedded(state) => {
                state.update_instance_data(instance_data);
                state.update_network_configuration(network_config);
            }
        }
    }
}

pub(super) struct FmdsGrpcClient {
    client: FmdsConfigServiceClient<Channel>,
    address: String,
    machine_identity: MachineIdentityConfig,
}

impl FmdsGrpcClient {
    pub(super) async fn connect(
        address: &str,
        machine_identity: MachineIdentityConfig,
        connect_timeout: Duration,
    ) -> eyre::Result<Self> {
        let channel = Endpoint::from_shared(address.to_string())
            .wrap_err_with(|| format!("invalid FMDS address {address}"))?
            .connect_timeout(connect_timeout)
            .connect()
            .await
            .wrap_err_with(|| format!("failed to connect to FMDS at {address}"))?;

        Ok(Self {
            client: FmdsConfigServiceClient::new(channel),
            address: address.to_string(),
            machine_identity,
        })
    }

    fn machine_identity_proto(&self) -> eyre::Result<FmdsMachineIdentityConfig> {
        MachineIdentityParams::try_from_limits(
            self.machine_identity.requests_per_second,
            self.machine_identity.burst,
            self.machine_identity.wait_timeout_secs,
            self.machine_identity.sign_timeout_secs,
            self.machine_identity.sign_proxy_url.as_deref(),
            self.machine_identity.sign_proxy_tls_root_ca.as_deref(),
        )
        .map(Into::into)
        .map_err(|msg| eyre!("machine-identity (FMDS config push): {msg}"))
    }

    async fn update_config(
        &mut self,
        instance_data: &Option<Arc<InstanceMetadata>>,
        network_config: &Option<Arc<ManagedHostNetworkConfigResponse>>,
    ) -> eyre::Result<()> {
        let Some(metadata) = instance_data else {
            return Ok(());
        };

        let asn = network_config.as_ref().map(|c| c.asn).unwrap_or(0);

        let ib_devices = metadata
            .ib_devices
            .as_ref()
            .map(|devices| {
                devices
                    .iter()
                    .map(|dev| IbDevice {
                        pf_guid: dev.pf_guid.clone(),
                        instances: dev
                            .instances
                            .iter()
                            .map(|inst| IbInstance {
                                ib_partition_id: inst
                                    .ib_partition_id
                                    .as_ref()
                                    .map(|id| id.to_string()),
                                ib_guid: inst.ib_guid.clone(),
                                lid: inst.lid,
                            })
                            .collect(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let update = FmdsConfigUpdate {
            address: metadata.public_addresses.ipv4_string(),
            address_ipv6: metadata.public_addresses.ipv6_string(),
            hostname: metadata.hostname.clone(),
            instance_name: metadata.instance_name.clone(),
            sitename: metadata.sitename.clone(),
            instance_id: metadata.instance_id,
            machine_id: metadata.machine_id,
            user_data: metadata.user_data.clone(),
            ib_devices,
            asn,
            machine_identity: Some(self.machine_identity_proto()?),
        };

        self.client
            .update_config(tonic::Request::new(UpdateConfigRequest {
                config_update: Some(update),
            }))
            .await?;

        tracing::debug!(
            fmds_address = self.address,
            "Sent config update to external FMDS"
        );

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::net::{Ipv4Addr, SocketAddr};

    use carbide_instrument::testing::MetricsCapture;
    use config_version::ConfigVersion;
    use rpc::fmds::UpdateConfigResponse;
    use rpc::fmds::fmds_config_service_server::{FmdsConfigService, FmdsConfigServiceServer};
    use tokio::net::{TcpListener, TcpSocket};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::{Request, Response, Status};

    use super::*;
    use crate::periodic_config_fetcher::PublicAddresses;

    /// Minimal FMDS server that records the updates it is sent.
    struct RecordingFmdsServer {
        updates: mpsc::UnboundedSender<FmdsConfigUpdate>,
        reject_updates: bool,
    }

    #[tonic::async_trait]
    impl FmdsConfigService for RecordingFmdsServer {
        async fn update_config(
            &self,
            request: Request<UpdateConfigRequest>,
        ) -> Result<Response<UpdateConfigResponse>, Status> {
            if self.reject_updates {
                return Err(Status::internal("test update rejection"));
            }

            let update = request
                .into_inner()
                .config_update
                .ok_or_else(|| Status::invalid_argument("missing config_update"))?;
            let _ = self.updates.send(update);
            Ok(Response::new(UpdateConfigResponse {}))
        }
    }

    /// Serves [`RecordingFmdsServer`] on an already-bound listener. The caller
    /// owns the returned handle and aborts it at the end of the test.
    fn serve_fmds(
        listener: TcpListener,
        reject_updates: bool,
    ) -> (
        SocketAddr,
        JoinHandle<()>,
        mpsc::UnboundedReceiver<FmdsConfigUpdate>,
    ) {
        let addr = listener.local_addr().expect("FMDS test server address");

        let (updates, received) = mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(FmdsConfigServiceServer::new(RecordingFmdsServer {
                    updates,
                    reject_updates,
                }))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .expect("FMDS test server");
        });

        (addr, handle, received)
    }

    fn test_instance_metadata() -> InstanceMetadata {
        InstanceMetadata {
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::new(192, 168, 1, 1)),
                ipv6: None,
            },
            hostname: "test-host".to_string(),
            instance_name: Some("test-instance".to_string()),
            sitename: Some("test-site".to_string()),
            instance_id: None,
            machine_id: None,
            user_data: "test-user-data".to_string(),
            ib_devices: None,
            config_version: ConfigVersion::initial(),
            network_config_version: ConfigVersion::initial(),
            extension_service_version: ConfigVersion::initial(),
        }
    }

    fn test_external_updater(address: String) -> (FmdsUpdater, Arc<AtomicBool>) {
        let last_connect_succeeded = Arc::new(AtomicBool::new(false));
        (
            FmdsUpdater::External {
                address,
                machine_identity: MachineIdentityConfig::default(),
                connect_timeout: Duration::from_millis(500),
                last_connect_succeeded: Arc::clone(&last_connect_succeeded),
            },
            last_connect_succeeded,
        )
    }

    async fn update_with_metrics_capture(updater: &mut FmdsUpdater) {
        let _metrics = MetricsCapture::start();
        updater
            .update(Some(Arc::new(test_instance_metadata())), None)
            .await;
    }

    /// The happy path through [`FmdsUpdater::update`]: it dials the external
    /// FMDS on every call and the update lands on the server.
    #[tokio::test]
    async fn external_updater_connects_and_pushes_every_update() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind FMDS test server");
        let (addr, server, mut received) = serve_fmds(listener, false);

        let (mut updater, _) = test_external_updater(format!("http://{addr}"));

        update_with_metrics_capture(&mut updater).await;

        let update = received.recv().await.expect("server received an update");
        assert_eq!(update.hostname, "test-host");
        assert_eq!(update.address, "192.168.1.1");
        assert!(update.machine_identity.is_some());

        // A second iteration reconnects and pushes again.
        update_with_metrics_capture(&mut updater).await;

        let update = received
            .recv()
            .await
            .expect("server received the second update");
        assert_eq!(update.hostname, "test-host");

        server.abort();
    }

    #[tokio::test]
    async fn external_updater_reports_connection_when_update_is_rejected() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind FMDS test server");
        let (addr, server, _) = serve_fmds(listener, true);

        let (mut updater, last_connect_succeeded) = test_external_updater(format!("http://{addr}"));
        update_with_metrics_capture(&mut updater).await;

        assert!(last_connect_succeeded.load(Ordering::Relaxed));

        server.abort();
    }

    /// The recovery this change exists for: an FMDS that is down when the agent
    /// starts is picked up by a later iteration, instead of the agent degrading
    /// for its whole lifetime.
    #[tokio::test]
    async fn external_updater_recovers_once_fmds_comes_up() {
        let socket = TcpSocket::new_v4().expect("create FMDS test socket");
        socket
            .bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .expect("bind FMDS test socket");
        let addr = socket.local_addr().expect("FMDS test server address");
        let (mut updater, last_connect_succeeded) = test_external_updater(format!("http://{addr}"));
        last_connect_succeeded.store(true, Ordering::Relaxed);

        // Nothing is listening yet. The push fails and is dropped, but the
        // updater stays usable rather than latching onto a degraded mode.
        update_with_metrics_capture(&mut updater).await;
        assert!(!last_connect_succeeded.load(Ordering::Relaxed));

        // FMDS shows up, and the next iteration reaches it.
        let listener = socket.listen(1024).expect("listen on FMDS test socket");
        let (_, server, mut received) = serve_fmds(listener, false);
        update_with_metrics_capture(&mut updater).await;
        assert!(last_connect_succeeded.load(Ordering::Relaxed));

        let update = received.recv().await.expect("server received an update");
        assert_eq!(update.hostname, "test-host");

        server.abort();
    }
}
