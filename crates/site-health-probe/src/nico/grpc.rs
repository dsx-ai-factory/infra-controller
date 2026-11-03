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

//! [`NicoApi`] over nico-api's gRPC surface, with the `carbide-rpc` TLS
//! client. Every method is a single read-only RPC with no connect retries,
//! so failures reach the reader's gate and its retry rule unchanged.

use std::sync::{Arc, Mutex, PoisonError};

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use forge_tls::client_config::ClientCert;
use rpc::forge::host_reprovisioning_list_response::HostReprovisioningListItem;
use rpc::forge_tls_client::{ForgeClientConfig, ForgeClientT, ForgeTlsClient};
use rpc::{forge, site_explorer};

use crate::config::NicoApiStateConfig;
use crate::gate::{CallError, classify_grpc};
use crate::nico::host_read::{HostDetail, full_host_detail};
use crate::nico::{NicoApi, NicoConnector};
use crate::probes::nicoapi::preflight_ca;

/// Dials nico-api with the state section's client certificate.
pub(crate) struct GrpcConnector {
    cfg: NicoApiStateConfig,
}

impl GrpcConnector {
    pub(crate) fn new(cfg: NicoApiStateConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait::async_trait]
impl NicoConnector for GrpcConnector {
    async fn connect(&self) -> Result<Arc<dyn NicoApi>, CallError> {
        preflight_ca(&self.cfg.tls.ca)
            .await
            .map_err(|e| CallError::Transport(format!("{e:#}")))?;
        let mut client_config = ForgeClientConfig::new(
            self.cfg.tls.ca.clone(),
            Some(ClientCert {
                cert_path: self.cfg.tls.cert.clone(),
                key_path: self.cfg.tls.key.clone(),
            }),
        );
        // Never an insecure mode, whatever DISABLE_TLS_ENFORCEMENT says.
        client_config.enforce_tls = true;
        // A host page of 100 full machine records can outgrow tonic's 4 MiB
        // default.
        client_config.max_decoding_message_size = Some(self.cfg.max_decoding_message_size);
        let client = ForgeTlsClient::new(&client_config)
            .build(format!("https://{}", self.cfg.target))
            .await
            .map_err(|e| CallError::Transport(format!("dial: {e}")))?;
        Ok(Arc::new(GrpcNico {
            client: Mutex::new(client),
        }))
    }
}

/// One sweep's nico-api session. Each call works on a clone of the client,
/// which shares the connection. The client is `Send` but not `Sync`, so the
/// shared original sits behind a mutex that is held only to clone it.
struct GrpcNico {
    client: Mutex<ForgeClientT>,
}

impl GrpcNico {
    fn client(&self) -> ForgeClientT {
        self.client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn grpc(status: tonic::Status) -> CallError {
    classify_grpc(&status)
}

#[async_trait::async_trait]
impl NicoApi for GrpcNico {
    async fn machine_ids(&self) -> Result<Vec<MachineId>, CallError> {
        let search = forge::MachineSearchConfig {
            include_dpus: true,
            include_predicted_host: false,
            ..Default::default()
        };
        let reply = self.client().find_machine_ids(search).await.map_err(grpc)?;
        Ok(reply.into_inner().machine_ids)
    }

    async fn hosts_by_ids(&self, ids: &[MachineId]) -> Result<Vec<HostDetail>, CallError> {
        let request = forge::MachinesByIdsRequest {
            machine_ids: ids.to_vec(),
            include_history: false,
        };
        let reply = self
            .client()
            .find_machines_by_ids(request)
            .await
            .map_err(grpc)?;
        reply
            .into_inner()
            .machines
            .into_iter()
            .map(full_host_detail)
            .collect()
    }

    async fn switch_ids(&self) -> Result<Vec<SwitchId>, CallError> {
        let reply = self
            .client()
            .find_switch_ids(forge::SwitchSearchFilter::default())
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().ids)
    }

    async fn switches_by_ids(&self, ids: &[SwitchId]) -> Result<Vec<forge::Switch>, CallError> {
        let request = forge::SwitchesByIdsRequest {
            switch_ids: ids.to_vec(),
        };
        let reply = self
            .client()
            .find_switches_by_ids(request)
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().switches)
    }

    async fn power_shelf_ids(&self) -> Result<Vec<PowerShelfId>, CallError> {
        let reply = self
            .client()
            .find_power_shelf_ids(forge::PowerShelfSearchFilter::default())
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().ids)
    }

    async fn power_shelves_by_ids(
        &self,
        ids: &[PowerShelfId],
    ) -> Result<Vec<forge::PowerShelf>, CallError> {
        let request = forge::PowerShelvesByIdsRequest {
            power_shelf_ids: ids.to_vec(),
        };
        let reply = self
            .client()
            .find_power_shelves_by_ids(request)
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().power_shelves)
    }

    async fn rack_ids(&self) -> Result<Vec<RackId>, CallError> {
        let reply = self
            .client()
            .find_rack_ids(forge::RackSearchFilter::default())
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().rack_ids)
    }

    async fn racks_by_ids(&self, ids: &[RackId]) -> Result<Vec<forge::Rack>, CallError> {
        let request = forge::RacksByIdsRequest {
            rack_ids: ids.to_vec(),
        };
        let reply = self
            .client()
            .find_racks_by_ids(request)
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().racks)
    }

    async fn instance_ids(&self) -> Result<Vec<InstanceId>, CallError> {
        let reply = self
            .client()
            .find_instance_ids(forge::InstanceSearchFilter::default())
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().instance_ids)
    }

    async fn instances_by_ids(
        &self,
        ids: &[InstanceId],
    ) -> Result<Vec<forge::Instance>, CallError> {
        let request = forge::InstancesByIdsRequest {
            instance_ids: ids.to_vec(),
        };
        let reply = self
            .client()
            .find_instances_by_ids(request)
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().instances)
    }

    async fn expected_machines_linked(
        &self,
    ) -> Result<Vec<forge::LinkedExpectedMachine>, CallError> {
        let reply = self
            .client()
            .get_all_expected_machines_linked(())
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().expected_machines)
    }

    async fn expected_switches_linked(
        &self,
    ) -> Result<Vec<forge::LinkedExpectedSwitch>, CallError> {
        let reply = self
            .client()
            .get_all_expected_switches_linked(())
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().expected_switches)
    }

    async fn expected_power_shelves_linked(
        &self,
    ) -> Result<Vec<forge::LinkedExpectedPowerShelf>, CallError> {
        let reply = self
            .client()
            .get_all_expected_power_shelves_linked(())
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().expected_power_shelves)
    }

    async fn expected_racks(&self) -> Result<Vec<forge::ExpectedRack>, CallError> {
        let reply = self
            .client()
            .get_all_expected_racks(())
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().expected_racks)
    }

    async fn expected_rack_groups(&self) -> Result<Vec<forge::ExpectedRackGroup>, CallError> {
        let reply = self
            .client()
            .get_all_expected_rack_groups(())
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().expected_rack_groups)
    }

    async fn hosts_waiting_for_reprovisioning(
        &self,
    ) -> Result<Vec<HostReprovisioningListItem>, CallError> {
        let reply = self
            .client()
            .list_hosts_waiting_for_reprovisioning(forge::HostReprovisioningListRequest {})
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().hosts)
    }

    async fn explored_endpoints(
        &self,
        addresses: &[String],
    ) -> Result<Vec<site_explorer::ExploredEndpoint>, CallError> {
        let request = site_explorer::ExploredEndpointsByIdsRequest {
            endpoint_ids: addresses.to_vec(),
        };
        let reply = self
            .client()
            .find_explored_endpoints_by_ids(request)
            .await
            .map_err(grpc)?;
        Ok(reply.into_inner().endpoints)
    }
}
