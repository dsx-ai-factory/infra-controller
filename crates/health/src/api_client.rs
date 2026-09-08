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

// The deprecated fields on `rpc::forge::Machine` must still be read here for
// backwards-compat. See https://github.com/NVIDIA/infra-controller/issues/2793
#![allow(deprecated)]

use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use carbide_uuid::nvlink::NvLinkDomainId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use forge_tls::client_config::ClientCert;
use mac_address::MacAddress;
use nv_redfish::bmc_http::reqwest::Client as ReqwestClient;
use rpc::forge::MachineSearchConfig;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use url::Url;

use crate::HealthError;
use crate::bmc::{
    BmcClient, BmcLatencyInstrumentation, BoxFuture, CredentialProvider,
    bmc_latency_endpoint_labels,
};
use crate::endpoint::{
    BmcAddr, BmcCredentials, BmcEndpoint, EndpointMetadata, EndpointSource, MachineData,
    PowerShelfData, SharedSystemUuid, SwitchData, SwitchEndpointRole,
};
use crate::metrics::BmcLatencyMetrics;

/// [`ApiEndpointSource`].
#[derive(Clone)]
pub struct ApiClientWrapper {
    client: ForgeApiClient,
}

impl ApiClientWrapper {
    pub fn new(root_ca: String, client_cert: String, client_key: String, api_url: &Url) -> Self {
        let client_config = ForgeClientConfig::new(
            root_ca,
            Some(ClientCert {
                cert_path: client_cert,
                key_path: client_key,
            }),
        );
        let api_config = ApiConfig::new(api_url.as_str(), &client_config);
        let client = ForgeApiClient::new(&api_config);

        Self { client }
    }

    pub async fn submit_health_report(
        &self,
        machine_id: &carbide_uuid::machine::MachineId,
        report: health_report::HealthReport,
    ) -> Result<(), HealthError> {
        let ovrd = rpc::forge::HealthReportEntry {
            report: Some(report.into()),
            mode: rpc::forge::HealthReportApplyMode::Merge.into(),
        };

        let request = rpc::forge::InsertMachineHealthReportRequest {
            machine_id: Some(*machine_id),
            health_report_entry: Some(ovrd),
        };

        self.client
            .insert_machine_health_report(request)
            .await
            .map_err(HealthError::ApiInvocationError)?;

        Ok(())
    }

    pub async fn submit_rack_health_report(
        &self,
        rack_id: &carbide_uuid::rack::RackId,
        report: health_report::HealthReport,
    ) -> Result<(), HealthError> {
        let ovrd = rpc::forge::HealthReportEntry {
            report: Some(report.into()),
            mode: rpc::forge::HealthReportApplyMode::Merge.into(),
        };

        let request = rpc::forge::InsertRackHealthReportRequest {
            rack_id: Some(rack_id.clone()),
            health_report_entry: Some(ovrd),
        };

        self.client
            .insert_rack_health_report(request)
            .await
            .map_err(HealthError::ApiInvocationError)?;

        Ok(())
    }

    pub async fn submit_switch_health_report(
        &self,
        switch_id: &carbide_uuid::switch::SwitchId,
        report: health_report::HealthReport,
    ) -> Result<(), HealthError> {
        let ovrd = rpc::forge::HealthReportEntry {
            report: Some(report.into()),
            mode: rpc::forge::HealthReportApplyMode::Merge.into(),
        };

        let request = rpc::forge::InsertSwitchHealthReportRequest {
            switch_id: Some(*switch_id),
            health_report_entry: Some(ovrd),
        };

        self.client
            .insert_switch_health_report(request)
            .await
            .map_err(HealthError::ApiInvocationError)?;

        Ok(())
    }

    /// Replaces one source's NVLink domain report using merge semantics.
    ///
    /// A success for a probe clears an alert from the same report source and
    /// probe identifier.
    pub async fn submit_nvlink_domain_health_report(
        &self,
        domain_id: &NvLinkDomainId,
        report: health_report::HealthReport,
    ) -> Result<(), HealthError> {
        let ovrd = rpc::forge::HealthReportEntry {
            report: Some(report.into()),
            mode: rpc::forge::HealthReportApplyMode::Merge.into(),
        };

        let request = rpc::forge::InsertNvLinkDomainHealthReportRequest {
            domain_id: Some(*domain_id),
            health_report_entry: Some(ovrd),
        };

        self.client
            .insert_nv_link_domain_health_report(request)
            .await
            .map_err(HealthError::ApiInvocationError)?;

        Ok(())
    }

    pub async fn submit_power_shelf_health_report(
        &self,
        power_shelf_id: &carbide_uuid::power_shelf::PowerShelfId,
        report: health_report::HealthReport,
    ) -> Result<(), HealthError> {
        let ovrd = rpc::forge::HealthReportEntry {
            report: Some(report.into()),
            mode: rpc::forge::HealthReportApplyMode::Merge.into(),
        };

        let request = rpc::forge::InsertPowerShelfHealthReportRequest {
            power_shelf_id: Some(*power_shelf_id),
            health_report_entry: Some(ovrd),
        };

        self.client
            .insert_power_shelf_health_report(request)
            .await
            .map_err(HealthError::ApiInvocationError)?;

        Ok(())
    }
    /// Fetch SKU manifests by id — the expected-hardware source of truth used
    /// to validate out-of-band GPU count against the assigned SKU.
    pub async fn find_skus_by_ids(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<rpc::forge::Sku>, HealthError> {
        let request = rpc::forge::SkusByIdsRequest { ids };

        let response = self
            .client
            .find_skus_by_ids(request)
            .await
            .map_err(HealthError::ApiInvocationError)?;

        Ok(response.skus)
    }

    /// Fetch a machine's currently-assigned SKU id, re-read live each call so SKU
    /// assignments/changes after a collector starts are picked up (no caching).
    pub async fn machine_hw_sku(
        &self,
        machine_id: carbide_uuid::machine::MachineId,
    ) -> Result<Option<String>, HealthError> {
        let request = rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![machine_id],
            ..Default::default()
        };

        let response = self
            .client
            .find_machines_by_ids(request)
            .await
            .map_err(HealthError::ApiInvocationError)?;

        Ok(response.machines.into_iter().next().and_then(|m| m.hw_sku))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ApiCredentialKind {
    Bmc,
    SwitchNvosAdmin { switch_id: SwitchId },
}

impl ApiCredentialKind {
    /// Short tag used in tracing fields.
    fn tag(&self) -> &'static str {
        match self {
            ApiCredentialKind::Bmc => "bmc",
            ApiCredentialKind::SwitchNvosAdmin { .. } => "switch-nvos-admin",
        }
    }
}

#[derive(Clone)]
struct ApiCredentialProvider {
    client: ForgeApiClient,
    kind: ApiCredentialKind,
}

impl CredentialProvider for ApiCredentialProvider {
    fn fetch_credentials<'a>(
        &'a self,
        endpoint: &'a BmcAddr,
    ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
        Box::pin(async move {
            let response = match &self.kind {
                ApiCredentialKind::Bmc => {
                    let request = rpc::forge::GetBmcCredentialsRequest {
                        mac_addr: endpoint.mac.to_string(),
                    };
                    self.client
                        .get_bmc_credentials(request)
                        .await
                        .map_err(HealthError::ApiInvocationError)?
                }
                ApiCredentialKind::SwitchNvosAdmin { switch_id } => {
                    let request = rpc::forge::GetSwitchNvosCredentialsRequest {
                        switch_id: Some(*switch_id),
                    };
                    self.client
                        .get_switch_nvos_credentials(request)
                        .await
                        .map_err(HealthError::ApiInvocationError)?
                }
            };

            response
                .credentials
                .and_then(|credentials| credentials.r#type)
                .map(Into::into)
                .ok_or_else(|| {
                    HealthError::GenericError("missing credentials in API response".to_string())
                })
        })
    }
}

fn switch_endpoint_metadata(
    switch: &rpc::forge::Switch,
    endpoint_role: SwitchEndpointRole,
    nmxt_enabled: bool,
) -> Result<EndpointMetadata, HealthError> {
    let config = switch
        .config
        .as_ref()
        .ok_or_else(|| HealthError::GenericError("switch endpoint does not have serial".into()))?;

    let serial = config.name.clone();

    Ok(EndpointMetadata::Switch(SwitchData {
        id: switch.id,
        serial,
        slot_number: switch
            .placement_in_rack
            .as_ref()
            .and_then(|placement| placement.slot_number),
        tray_index: switch
            .placement_in_rack
            .as_ref()
            .and_then(|placement| placement.tray_index),
        nvlink_domain_uuid: switch
            .nvlink_domain_uuid
            .filter(|domain_uuid| domain_uuid != &NvLinkDomainId::nil()),
        endpoint_role,
        is_primary: switch.is_primary,
        nmxc_enabled: config.enable_nmxc || switch.is_primary,
        nmxt_enabled,
    }))
}

pub struct ApiEndpointSource {
    api: Arc<ApiClientWrapper>,
    reqwest: ReqwestClient,
    proxy_url: Option<Url>,
    cache_size: usize,
    bmc_request_concurrency: NonZeroUsize,
    bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
    bmc_client_cache: Mutex<HashMap<MacAddress, CachedBmcClient>>,
}

#[derive(Clone)]
struct CachedBmcClient {
    client: Arc<BmcClient>,
    kind: ApiCredentialKind,
    system_uuid: SharedSystemUuid,
}

impl ApiEndpointSource {
    pub fn new(
        api: Arc<ApiClientWrapper>,
        reqwest: ReqwestClient,
        proxy_url: Option<Url>,
        cache_size: usize,
        bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
    ) -> Self {
        Self::new_with_request_concurrency(
            api,
            reqwest,
            proxy_url,
            cache_size,
            NonZeroUsize::MIN,
            bmc_latency_metrics,
        )
    }

    pub(crate) fn new_with_request_concurrency(
        api: Arc<ApiClientWrapper>,
        reqwest: ReqwestClient,
        proxy_url: Option<Url>,
        cache_size: usize,
        bmc_request_concurrency: NonZeroUsize,
        bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
    ) -> Self {
        Self {
            api,
            reqwest,
            proxy_url,
            cache_size,
            bmc_request_concurrency,
            bmc_latency_metrics,
            bmc_client_cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn fetch_bmc_hosts(&self) -> Result<Vec<Arc<BmcEndpoint>>, HealthError> {
        let mut endpoints = self.fetch_machine_endpoints().await?;
        endpoints.extend(self.fetch_power_shelf_endpoints().await);
        endpoints.extend(self.fetch_switch_endpoints().await);

        self.prune_bmc_client_cache(&endpoints);

        tracing::info!(endpoint_count = endpoints.len(), "Prepared endpoints");

        Ok(endpoints)
    }

    fn prune_bmc_client_cache(&self, live_endpoints: &[Arc<BmcEndpoint>]) {
        let live_macs: HashSet<MacAddress> = live_endpoints.iter().map(|ep| ep.addr.mac).collect();
        let mut cache = self.bmc_client_cache.lock().expect("cache mutex poisoned");
        let before = cache.len();
        cache.retain(|mac, _| live_macs.contains(mac));
        let removed = before - cache.len();
        if removed > 0 {
            tracing::info!(
                removed_bmc_client_count = removed,
                remaining_bmc_client_count = cache.len(),
                "Pruned stale BmcClient cache entries"
            );
        }
    }

    async fn fetch_machine_endpoints(&self) -> Result<Vec<Arc<BmcEndpoint>>, HealthError> {
        let machine_ids = self
            .api
            .client
            .find_machine_ids(MachineSearchConfig {
                include_dpus: true,
                ..Default::default()
            })
            .await
            .map_err(HealthError::ApiInvocationError)?;

        tracing::info!(
            machine_count = machine_ids.machine_ids.len(),
            "Found machines"
        );

        let mut endpoints = Vec::new();

        for ids_chunk in machine_ids.machine_ids.chunks(100) {
            let request = ::rpc::forge::MachinesByIdsRequest {
                machine_ids: Vec::from(ids_chunk),
                ..Default::default()
            };
            let machines = self
                .api
                .client
                .find_machines_by_ids(request)
                .await
                .map_err(HealthError::ApiInvocationError)?;
            tracing::debug!(
                machine_count = machines.machines.len(),
                requested_machine_count = ids_chunk.len(),
                "Fetched machine details"
            );

            for machine in machines.machines {
                match self.extract_machine_endpoint(&machine) {
                    Ok(endpoint) => endpoints.push(endpoint),
                    Err(error) => tracing::warn!(
                        ?machine,
                        ?error,
                        rack_id = machine.rack_id.as_ref().map(tracing::field::display),
                        "Could not add machine endpoint due to error"
                    ),
                }
            }
        }

        Ok(endpoints)
    }

    async fn fetch_switch_endpoints(&self) -> Vec<Arc<BmcEndpoint>> {
        let switch_request = rpc::forge::SwitchQuery {
            name: None,
            switch_id: None,
        };

        match self.api.client.find_switches(switch_request).await {
            Ok(response) => {
                let mut endpoints = Vec::new();

                for switch in response.switches {
                    match self.extract_switch_endpoint(&switch) {
                        Ok(endpoint) => endpoints.push(endpoint),
                        Err(error) => tracing::warn!(
                            ?switch,
                            ?error,
                            rack_id = switch.rack_id.as_ref().map(tracing::field::display),
                            "Could not add switch endpoint due to error"
                        ),
                    }

                    match self.extract_switch_host_endpoint(&switch) {
                        Ok(Some(endpoint)) => endpoints.push(endpoint),
                        Ok(None) => {}
                        Err(error) => tracing::warn!(
                            ?switch,
                            ?error,
                            rack_id = switch.rack_id.as_ref().map(tracing::field::display),
                            "Could not add switch host endpoint due to error"
                        ),
                    }
                }

                tracing::debug!(
                    switch_endpoint_count = endpoints.len(),
                    "Fetched switch endpoints"
                );
                endpoints
            }
            Err(error) => {
                tracing::warn!(?error, "Failed to fetch switch endpoints");
                Vec::new()
            }
        }
    }

    async fn fetch_power_shelf_endpoints(&self) -> Vec<Arc<BmcEndpoint>> {
        let request = rpc::forge::PowerShelfQuery {
            name: None,
            power_shelf_id: None,
        };

        match self.api.client.find_power_shelves(request).await {
            Ok(response) => {
                let mut endpoints = Vec::new();

                for power_shelf in response.power_shelves {
                    match self.extract_power_shelf_endpoint(&power_shelf) {
                        Ok(endpoint) => endpoints.push(endpoint),
                        Err(error) => tracing::warn!(
                            ?power_shelf,
                            ?error,
                            rack_id = power_shelf.rack_id.as_ref().map(tracing::field::display),
                            "Could not add power shelf endpoint due to error"
                        ),
                    }
                }

                tracing::debug!(
                    power_shelf_endpoint_count = endpoints.len(),
                    "Fetched power shelf endpoints"
                );
                endpoints
            }
            Err(error) => {
                tracing::warn!(?error, "Failed to fetch power shelf endpoints");
                Vec::new()
            }
        }
    }

    fn extract_machine_endpoint(
        &self,
        machine: &rpc::forge::Machine,
    ) -> Result<Arc<BmcEndpoint>, HealthError> {
        let Some(bmc_info) = &machine.bmc_info else {
            return Err(HealthError::GenericError(
                "Could not extract machine endpoint without BMC Info".to_string(),
            ));
        };
        let addr = BmcAddr::try_from(bmc_info)?;
        let metadata = machine.id.map(|machine_id| {
            EndpointMetadata::Machine(MachineData {
                machine_id: Some(machine_id),
                machine_serial: machine
                    .discovery_info
                    .as_ref()
                    .and_then(|info| info.dmi_data.as_ref())
                    .map(|dmi| dmi.chassis_serial.clone()),
                system_uuid: SharedSystemUuid::default(),
                slot_number: machine
                    .placement_in_rack
                    .as_ref()
                    .and_then(|placement| placement.slot_number),
                tray_index: machine
                    .placement_in_rack
                    .as_ref()
                    .and_then(|placement| placement.tray_index),
                nvlink_domain_uuid: machine
                    .nvlink_info
                    .as_ref()
                    .and_then(|info| info.domain_uuid),
                driver_version: unique_gpu_driver_version(machine.discovery_info.as_ref()),
            })
        });

        self.endpoint_for(
            addr,
            metadata,
            machine.rack_id.clone(),
            ApiCredentialKind::Bmc,
        )
    }

    fn extract_switch_endpoint(
        &self,
        switch: &rpc::forge::Switch,
    ) -> Result<Arc<BmcEndpoint>, HealthError> {
        let Some(bmc_info) = &switch.bmc_info else {
            return Err(HealthError::GenericError(
                "Could not extract switch endpoint without BMC Info".to_string(),
            ));
        };
        let addr = BmcAddr::try_from(bmc_info)?;
        let metadata = switch_endpoint_metadata(switch, SwitchEndpointRole::Bmc, false)?;

        self.endpoint_for(
            addr,
            Some(metadata),
            switch.rack_id.clone(),
            ApiCredentialKind::Bmc,
        )
    }

    fn extract_switch_host_endpoint(
        &self,
        switch: &rpc::forge::Switch,
    ) -> Result<Option<Arc<BmcEndpoint>>, HealthError> {
        let Some(nvos_info) = switch.nvos_info.as_ref() else {
            return Ok(None);
        };
        let switch_id = switch.id.ok_or_else(|| {
            HealthError::GenericError("switch host endpoint missing switch ID".to_string())
        })?;
        let addr = BmcAddr::try_from(nvos_info)?;
        let metadata =
            switch_endpoint_metadata(switch, SwitchEndpointRole::Host, switch.is_primary)?;

        let endpoint = self.endpoint_for(
            addr,
            Some(metadata),
            switch.rack_id.clone(),
            ApiCredentialKind::SwitchNvosAdmin { switch_id },
        )?;

        Ok(Some(endpoint))
    }

    fn extract_power_shelf_endpoint(
        &self,
        power_shelf: &rpc::forge::PowerShelf,
    ) -> Result<Arc<BmcEndpoint>, HealthError> {
        let Some(bmc_info) = &power_shelf.bmc_info else {
            return Err(HealthError::GenericError(
                "Could not extract power shelf endpoint without BMC Info".to_string(),
            ));
        };
        let addr = BmcAddr::try_from(bmc_info)?;

        self.endpoint_for(
            addr,
            Some(EndpointMetadata::PowerShelf(PowerShelfData {
                id: power_shelf.id,
                serial: None,
            })),
            power_shelf.rack_id.clone(),
            ApiCredentialKind::Bmc,
        )
    }

    fn endpoint_for(
        &self,
        addr: BmcAddr,
        metadata: Option<EndpointMetadata>,
        rack_id: Option<RackId>,
        credential_kind: ApiCredentialKind,
    ) -> Result<Arc<BmcEndpoint>, HealthError> {
        let bmc_latency_instrumentation = self.bmc_latency_metrics.clone().map(|metrics| {
            BmcLatencyInstrumentation::new(
                metrics,
                bmc_latency_endpoint_labels(metadata.as_ref(), rack_id.as_ref()),
            )
        });
        let cached = {
            let mut cache = self.bmc_client_cache.lock().expect("cache mutex poisoned");
            cache_or_create_bmc_client(&mut cache, addr.mac, credential_kind, |kind| {
                let provider: Arc<dyn CredentialProvider> = Arc::new(ApiCredentialProvider {
                    client: self.api.client.clone(),
                    kind,
                });
                Ok(Arc::new(BmcClient::new(
                    self.reqwest.clone(),
                    addr.clone(),
                    provider,
                    self.proxy_url.clone(),
                    self.cache_size,
                    self.bmc_request_concurrency,
                    bmc_latency_instrumentation,
                )?))
            })?
        };
        let mut metadata = metadata;
        if let Some(EndpointMetadata::Machine(machine)) = metadata.as_mut() {
            machine.system_uuid = cached.system_uuid;
        }
        Ok(Arc::new(BmcEndpoint {
            addr,
            metadata,
            rack_id,
            labels: Default::default(),
            bmc: cached.client,
        }))
    }
}

fn cache_or_create_bmc_client(
    cache: &mut HashMap<MacAddress, CachedBmcClient>,
    mac: MacAddress,
    credential_kind: ApiCredentialKind,
    make_client: impl FnOnce(ApiCredentialKind) -> Result<Arc<BmcClient>, HealthError>,
) -> Result<CachedBmcClient, HealthError> {
    if let Some(existing) = cache.get(&mac) {
        if existing.kind != credential_kind {
            return Err(HealthError::GenericError(format!(
                "MAC {mac} reported with conflicting credential kinds \
                 (cached={}, requested={}); refusing to construct endpoint \
                 so we don't rotate credentials with the wrong RPC",
                existing.kind.tag(),
                credential_kind.tag(),
            )));
        }
        return Ok(existing.clone());
    }

    let client = make_client(credential_kind.clone())?;
    let cached = CachedBmcClient {
        client,
        kind: credential_kind,
        system_uuid: SharedSystemUuid::default(),
    };
    cache.insert(mac, cached.clone());
    Ok(cached)
}

/// Returns the machine-level GPU driver version derived from discovery data.
///
/// The NICo API reports driver versions per GPU. Health emits one machine-level
/// value only when there is exactly one unique non-empty version across the
/// reported GPUs. Empty strings are treated as missing data; conflicting
/// non-empty versions are treated as ambiguous and omitted.
fn unique_gpu_driver_version(
    discovery_info: Option<&rpc::machine_discovery::DiscoveryInfo>,
) -> Option<String> {
    let discovery_info = discovery_info?;
    let versions = discovery_info
        .gpus
        .iter()
        .map(|gpu| gpu.driver_version.trim())
        .filter(|version| !version.is_empty())
        .map(str::to_string)
        .collect::<HashSet<_>>();

    (versions.len() == 1)
        .then(|| versions.into_iter().next())
        .flatten()
}

impl EndpointSource for ApiEndpointSource {
    fn fetch_bmc_hosts<'a>(&'a self) -> BoxFuture<'a, Result<Vec<Arc<BmcEndpoint>>, HealthError>> {
        Box::pin(self.fetch_bmc_hosts())
    }
}

impl TryFrom<&rpc::forge::BmcInfo> for BmcAddr {
    type Error = HealthError;

    fn try_from(bmc_info: &rpc::forge::BmcInfo) -> Result<Self, Self::Error> {
        let ip = bmc_info
            .ip
            .as_ref()
            .ok_or_else(|| HealthError::GenericError("missing BMC IP address".to_string()))?
            .parse::<IpAddr>()
            .map_err(|error| HealthError::GenericError(error.to_string()))?;
        let mac = bmc_info
            .mac
            .as_ref()
            .ok_or_else(|| HealthError::GenericError("missing BMC MAC address".to_string()))
            .and_then(|mac| {
                MacAddress::from_str(mac)
                    .map_err(|error| HealthError::GenericError(error.to_string()))
            })?;
        let port = bmc_info.port.map(|port| port.try_into().unwrap_or(443));

        Ok(Self { ip, port, mac })
    }
}

impl TryFrom<&rpc::forge::SwitchNvosInfo> for BmcAddr {
    type Error = HealthError;

    fn try_from(nvos_info: &rpc::forge::SwitchNvosInfo) -> Result<Self, Self::Error> {
        let ip = nvos_info
            .ip
            .as_ref()
            .ok_or_else(|| HealthError::GenericError("missing NVOS IP address".to_string()))?
            .parse::<IpAddr>()
            .map_err(|error| HealthError::GenericError(error.to_string()))?;
        let mac = nvos_info
            .mac
            .as_ref()
            .ok_or_else(|| HealthError::GenericError("missing NVOS MAC address".to_string()))
            .and_then(|mac| {
                MacAddress::from_str(mac)
                    .map_err(|error| HealthError::GenericError(error.to_string()))
            })?;
        let port = nvos_info.port.map(|port| port.try_into().unwrap_or(443));

        Ok(Self { ip, port, mac })
    }
}

impl From<rpc::forge::UsernamePassword> for BmcCredentials {
    fn from(value: rpc::forge::UsernamePassword) -> Self {
        Self::UsernamePassword {
            username: value.username,
            password: Some(value.password),
        }
    }
}

impl From<rpc::forge::SessionToken> for BmcCredentials {
    fn from(value: rpc::forge::SessionToken) -> Self {
        Self::SessionToken { token: value.token }
    }
}

impl From<rpc::forge::bmc_credentials::Type> for BmcCredentials {
    fn from(value: rpc::forge::bmc_credentials::Type) -> Self {
        match value {
            rpc::forge::bmc_credentials::Type::UsernamePassword(value) => value.into(),
            rpc::forge::bmc_credentials::Type::SessionToken(value) => value.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_test_support::{Check, check_values, value_scenarios};
    use carbide_uuid::nvlink::NvLinkDomainId;
    use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
    use nv_redfish::bmc_http::reqwest::ClientParams as ReqwestClientParams;

    use super::*;
    use crate::bmc::FixedCredentialProvider;

    fn test_mac() -> MacAddress {
        MacAddress::from_str("00:11:22:33:44:55").expect("valid mac")
    }

    fn test_addr() -> BmcAddr {
        BmcAddr {
            ip: "10.0.0.1".parse().expect("valid ip"),
            port: Some(443),
            mac: test_mac(),
        }
    }

    fn reqwest() -> ReqwestClient {
        ReqwestClient::with_params(ReqwestClientParams::new().accept_invalid_certs(true))
            .expect("reqwest client builds")
    }

    fn test_switch_id() -> SwitchId {
        SwitchId::new(SwitchIdSource::Tpm, [7u8; 32], SwitchType::NvLink)
    }

    fn make_test_client(_kind: ApiCredentialKind) -> Result<Arc<BmcClient>, HealthError> {
        let provider = Arc::new(FixedCredentialProvider::new(BmcCredentials::SessionToken {
            token: "t".to_string(),
        }));
        Ok(Arc::new(BmcClient::new(
            reqwest(),
            test_addr(),
            provider,
            None,
            10,
            NonZeroUsize::MIN,
            None,
        )?))
    }

    /// Builds discovery metadata with one GPU entry per supplied driver version.
    fn discovery_with_driver_versions(
        driver_versions: &[&str],
    ) -> rpc::machine_discovery::DiscoveryInfo {
        rpc::machine_discovery::DiscoveryInfo {
            gpus: driver_versions
                .iter()
                .map(|driver_version| rpc::machine_discovery::Gpu {
                    driver_version: (*driver_version).to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// Verifies that driver-version extraction emits only a unique non-empty value.
    #[test]
    fn unique_gpu_driver_version_uses_single_non_empty_version() {
        value_scenarios!(
            run = |discovery_info: Option<rpc::machine_discovery::DiscoveryInfo>| {
                unique_gpu_driver_version(discovery_info.as_ref())
            };
            "missing discovery info" {
                None => None,
            }

            "no gpus" {
                Some(discovery_with_driver_versions(&[])) => None,
            }

            "empty gpu driver versions" {
                Some(discovery_with_driver_versions(&["", "  "])) => None,
            }

            "one gpu driver version" {
                Some(discovery_with_driver_versions(&["570.82"])) => Some("570.82".to_string()),
            }

            "same gpu driver version repeated" {
                Some(discovery_with_driver_versions(&["570.82", " 570.82 "])) => {
                    Some("570.82".to_string())
                },
            }

            "mixed gpu driver versions" {
                Some(discovery_with_driver_versions(&["570.82", "580.12"])) => None,
            }
        );
    }

    #[test]
    fn switch_endpoint_metadata_uses_non_nil_api_domain() {
        let domain = NvLinkDomainId::from_str("9f4b45ec-705a-4af4-89f7-a112bc9c8f4e")
            .expect("valid domain UUID");

        check_values(
            [
                Check {
                    scenario: "domain is missing",
                    input: None,
                    expect: None,
                },
                Check {
                    scenario: "nil domain is absent",
                    input: Some(NvLinkDomainId::nil()),
                    expect: None,
                },
                Check {
                    scenario: "non-nil API switch field",
                    input: Some(domain),
                    expect: Some(domain),
                },
            ],
            |nvlink_domain_uuid| {
                let metadata = switch_endpoint_metadata(
                    &rpc::forge::Switch {
                        config: Some(rpc::forge::SwitchConfig {
                            name: "switch-a".to_string(),
                            ..Default::default()
                        }),
                        nvlink_domain_uuid,
                        ..Default::default()
                    },
                    SwitchEndpointRole::Bmc,
                    false,
                )
                .expect("switch metadata");

                let EndpointMetadata::Switch(switch) = metadata else {
                    panic!("expected switch metadata");
                };

                switch.nvlink_domain_uuid
            },
        );
    }

    #[test]
    fn switch_endpoint_metadata_enables_nmxc_for_primary_switch() {
        let metadata = switch_endpoint_metadata(
            &rpc::forge::Switch {
                config: Some(rpc::forge::SwitchConfig {
                    name: "switch-a".to_string(),
                    ..Default::default()
                }),
                is_primary: true,
                ..Default::default()
            },
            SwitchEndpointRole::Host,
            false,
        )
        .expect("switch metadata");

        let EndpointMetadata::Switch(switch) = metadata else {
            panic!("expected switch metadata");
        };

        assert!(switch.nmxc_enabled);
    }

    #[test]
    fn power_shelf_endpoint_preserves_api_rack_id()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let api_url = Url::parse("https://127.0.0.1:1079")?;

        let source = ApiEndpointSource::new(
            Arc::new(ApiClientWrapper::new(
                "test-ca.pem".to_string(),
                "test-client.pem".to_string(),
                "test-client-key.pem".to_string(),
                &api_url,
            )),
            reqwest(),
            None,
            10,
            None,
        );

        let rack_id = RackId::new("RACK_1");

        let endpoint = source.extract_power_shelf_endpoint(&rpc::forge::PowerShelf {
            config: Some(rpc::forge::PowerShelfConfig {
                name: "power-shelf-a".to_string(),
                ..Default::default()
            }),
            bmc_info: Some(rpc::forge::BmcInfo {
                ip: Some("10.0.0.1".to_string()),
                mac: Some(test_mac().to_string()),
                port: Some(443),
                ..Default::default()
            }),
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        })?;

        assert_eq!(endpoint.rack_id.as_ref(), Some(&rack_id));
        let Some(EndpointMetadata::PowerShelf(power_shelf)) = endpoint.metadata.as_ref() else {
            panic!("expected power shelf metadata");
        };
        assert_eq!(power_shelf.serial, None);

        Ok(())
    }

    #[tokio::test]
    async fn cache_returns_existing_client_on_matching_kind() {
        let mut cache: HashMap<MacAddress, CachedBmcClient> = HashMap::new();
        let factory_calls = AtomicUsize::new(0);

        let first =
            cache_or_create_bmc_client(&mut cache, test_mac(), ApiCredentialKind::Bmc, |kind| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                make_test_client(kind)
            })
            .expect("first insert");

        let second =
            cache_or_create_bmc_client(&mut cache, test_mac(), ApiCredentialKind::Bmc, |kind| {
                factory_calls.fetch_add(1, Ordering::SeqCst);
                make_test_client(kind)
            })
            .expect("cache hit");

        assert!(
            Arc::ptr_eq(&first.client, &second.client),
            "cache hit must reuse the same BmcClient Arc — otherwise every \
             iteration of discovery rebuilds the session and re-fetches creds"
        );
        let system_uuid = uuid::uuid!("4c4c4544-0044-4710-8052-cac04f4b4632");
        first
            .system_uuid
            .get_or_try_init(|| async { Ok::<_, std::convert::Infallible>(Some(system_uuid)) })
            .await
            .expect("infallible UUID initialization");
        assert_eq!(
            second.system_uuid.get(),
            Some(system_uuid),
            "cache hit must reuse machine UUID state across discovery iterations"
        );
        assert_eq!(
            factory_calls.load(Ordering::SeqCst),
            1,
            "factory must only be called on cache miss"
        );
    }

    #[test]
    fn cache_rejects_conflicting_credential_kinds() {
        let mut cache: HashMap<MacAddress, CachedBmcClient> = HashMap::new();

        cache_or_create_bmc_client(
            &mut cache,
            test_mac(),
            ApiCredentialKind::Bmc,
            make_test_client,
        )
        .expect("first insert ok");

        let err = cache_or_create_bmc_client(
            &mut cache,
            test_mac(),
            ApiCredentialKind::SwitchNvosAdmin {
                switch_id: test_switch_id(),
            },
            |_| panic!("factory must not be invoked when the cache mismatch is detected"),
        )
        .map(|_| ())
        .expect_err("mismatched credential kind must error out");

        match err {
            HealthError::GenericError(msg) => {
                assert!(
                    msg.contains("conflicting credential kinds"),
                    "expected mismatch message, got: {msg}"
                );
                assert!(msg.contains(ApiCredentialKind::Bmc.tag()));
                assert!(
                    msg.contains(
                        ApiCredentialKind::SwitchNvosAdmin {
                            switch_id: test_switch_id()
                        }
                        .tag()
                    )
                );
            }
            other => panic!("unexpected error variant: {other:?}"),
        }

        let still_cached = cache
            .get(&test_mac())
            .expect("cached entry not removed by failed registration");
        assert_eq!(still_cached.kind, ApiCredentialKind::Bmc);
    }
}
