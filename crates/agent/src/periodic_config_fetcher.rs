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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use ::rpc::carbide_tls_client::CarbideClientConfig;
use ::rpc::{Instance, forge as rpc};
use arc_swap::{ArcSwap, ArcSwapOption};
use carbide_network::ip::{IdentifyAddressFamily, IpAddressFamily};
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineInterfaceId};
use config_version::ConfigVersion;
use eyre::Context;
use forge_dpu_agent_utils::utils::create_forge_client;
use ipnetwork::IpNetwork;
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::instrumentation::ConfigFetch;
use crate::service_network;
use crate::util::{get_periodic_dpu_config, get_sitename, jittered_interval};

/// What the fetcher knows about this DPU's desired network configuration.
///
/// A plain `Option` collapses two unrelated conditions into `None`: Core
/// answered `NotFound`, and Core did not answer at all. Only the first means
/// the DPU was deleted, so they are kept apart here.
#[derive(Clone, Debug, Default)]
pub(super) enum NetworkConfigState {
    /// A fetch succeeded. This is the configuration Core returned.
    Known(Arc<rpc::ManagedHostNetworkConfigResponse>),
    /// Core answered `NotFound`. The DPU was deleted.
    Absent,
    /// No fetch has succeeded in this process yet, or the last attempt failed
    /// before Core could answer. Nothing may be inferred from this: in
    /// particular it is not evidence that the DPU was deleted.
    #[default]
    Unknown,
}

impl NetworkConfigState {
    /// Whether this state is evidence that the DPU's networking must be
    /// blanked.
    ///
    /// `Absent` is that evidence for every DPU: Core was reachable and said
    /// this DPU is gone. `Unknown` is not evidence of anything, but a DPU
    /// without `retain_hbn_config_on_boot` still blanks on it, which is the
    /// behavior that keeps a stale DPU off the network. A DPU with the flag
    /// holds its configuration instead, so its host can boot while NICo is
    /// unavailable. The flag therefore only changes what an unanswered fetch
    /// means; it never overrides a `NotFound`.
    /// The configuration Core returned, if there is one to act on.
    pub(super) fn known(&self) -> Option<&Arc<rpc::ManagedHostNetworkConfigResponse>> {
        match self {
            Self::Known(conf) => Some(conf),
            Self::Absent | Self::Unknown => None,
        }
    }

    pub(super) fn requires_reset(&self, retain_hbn_config: bool) -> bool {
        match self {
            Self::Known(_) => false,
            Self::Absent => true,
            Self::Unknown => !retain_hbn_config,
        }
    }
}

struct PeriodicFetcherState {
    config: PeriodicConfigFetcherConfig,
    netconf: ArcSwap<NetworkConfigState>,
    instmeta: ArcSwapOption<InstanceMetadata>,
    cancel_token: CancellationToken,
    sitename: Option<String>,
}

/// Fetches the desired network configuration for a managed host with jittered waits.
pub(super) struct PeriodicConfigFetcher {
    state: Arc<PeriodicFetcherState>,
    join_handle: Option<tokio::task::JoinHandle<()>>,
}

pub(super) struct PeriodicConfigFetcherReader {
    state: Arc<PeriodicFetcherState>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PublicAddresses {
    pub(super) ipv4: Option<Ipv4Addr>,
    pub(super) ipv6: Option<Ipv6Addr>,
}

impl PublicAddresses {
    /// Preserves FMDS's empty-string contract when no IPv4 address is assigned.
    pub(super) fn ipv4_string(self) -> String {
        self.ipv4
            .map(|address| address.to_string())
            .unwrap_or_default()
    }

    /// Preserves FMDS's empty-string contract when no IPv6 address is assigned.
    pub(super) fn ipv6_string(self) -> String {
        self.ipv6
            .map(|address| address.to_string())
            .unwrap_or_default()
    }
}

/// The instance metadata - as fetched from the
/// Forge Site Controller
#[derive(Clone, Debug)]
pub(super) struct InstanceMetadata {
    pub(super) public_addresses: PublicAddresses,
    pub(super) hostname: String,
    pub(super) instance_name: Option<String>,
    pub(super) sitename: Option<String>,
    pub(super) instance_id: Option<InstanceId>,
    pub(super) machine_id: Option<MachineId>,
    pub(super) user_data: String,
    pub(super) ib_devices: Option<Vec<IBDeviceConfig>>,
    pub(super) config_version: ConfigVersion,
    pub(super) network_config_version: ConfigVersion,
    pub(super) extension_service_version: ConfigVersion,
}

#[derive(Clone, Debug)]
pub(super) struct IBDeviceConfig {
    pub(super) pf_guid: String,
    pub(super) instances: Vec<IBInstanceConfig>,
}

#[derive(Clone, Debug)]
pub(super) struct IBInstanceConfig {
    pub(super) ib_partition_id: Option<IBPartitionId>,
    pub(super) ib_guid: Option<String>,
    pub(super) lid: u32,
}

impl PeriodicConfigFetcherReader {
    /// The configuration Core returned, if a fetch has ever succeeded and the
    /// DPU has not since been deleted. Callers that must distinguish "deleted"
    /// from "Core did not answer" use [`Self::net_conf_state`] instead.
    pub(super) fn net_conf_read(&self) -> Option<Arc<rpc::ManagedHostNetworkConfigResponse>> {
        self.state.netconf.load().known().map(Arc::clone)
    }

    pub(super) fn net_conf_state(&self) -> NetworkConfigState {
        NetworkConfigState::clone(&self.state.netconf.load())
    }

    pub(super) fn meta_data_conf_reader(&self) -> Option<Arc<InstanceMetadata>> {
        self.state.instmeta.load_full()
    }
}

impl Drop for PeriodicConfigFetcher {
    fn drop(&mut self) {
        // Interrupt the wait; any fetch already in progress may finish before joining.
        self.state.cancel_token.cancel();
        if let Some(jh) = self.join_handle.take() {
            tokio::spawn(async move {
                jh.await.unwrap();
            });
        }
    }
}

impl PeriodicConfigFetcher {
    pub(super) async fn new(config: PeriodicConfigFetcherConfig) -> Self {
        let carbide_client_config = Arc::clone(&config.carbide_client_config);
        // Fetch the sitename from Carbide at the start and keep it in State
        // so that it can be made available as instance metadata.
        let sitename = match fetch_sitename(&carbide_client_config, &config.forge_api).await {
            Ok(sn) => sn,
            Err(e) => {
                warn!(error = %e, "Unable to fetch sitename");
                None
            }
        };

        let state = Arc::new(PeriodicFetcherState {
            netconf: ArcSwap::default(),
            instmeta: ArcSwapOption::default(),
            sitename,
            config,
            cancel_token: CancellationToken::new(),
        });

        // Do an initial synchronous fetch so that caller has data to use
        // This gets a DPU on the network immediately
        let next_fetch_delay = single_fetch(&carbide_client_config, state.clone(), true).await;

        let task_state = state.clone();
        let join_handle = tokio::spawn(async move {
            let mut next_fetch_delay = next_fetch_delay;
            while let Some(delay) = next_fetch_delay {
                tokio::select! {
                    biased;
                    () = task_state.cancel_token.cancelled() => break,
                    () = tokio::time::sleep(delay) => {}
                }
                next_fetch_delay =
                    single_fetch(&carbide_client_config, task_state.clone(), false).await;
            }
        });

        Self {
            state,
            join_handle: Some(join_handle),
        }
    }

    pub(super) fn reader(&self) -> Box<PeriodicConfigFetcherReader> {
        Box::new(PeriodicConfigFetcherReader {
            state: self.state.clone(),
        })
    }

    pub(super) fn get_host_machine_interface_id(&self) -> Option<MachineInterfaceId> {
        self.state
            .netconf
            .load()
            .known()
            .and_then(|netconf| netconf.host_interface_id.as_ref())
            .and_then(|id| id.parse().ok())
    }
}

pub(super) struct PeriodicConfigFetcherConfig {
    /// Wait between recurring fetches, plus up to 10% jitter. An initial
    /// fetch failure gets one immediate background retry.
    pub(super) config_fetch_interval: Duration,
    pub(super) machine_id: DpuMachineId,
    pub(super) forge_api: String,
    pub(super) carbide_client_config: Arc<CarbideClientConfig>,
}

// Use the version grpc call to carbide to get
// the sitename. This will be made visible to tenant OS
// at an FMDS endpoint
async fn fetch_sitename(
    carbide_client_config: &CarbideClientConfig,
    forge_api: &str,
) -> Result<Option<String>, eyre::Report> {
    let mut client = create_forge_client(forge_api, carbide_client_config).await?;

    get_sitename(&mut client).await
}

/// Returns the next fetch delay, or `None` when the fetcher was dropped.
async fn single_fetch(
    carbide_client_config: &CarbideClientConfig,
    state: Arc<PeriodicFetcherState>,
    initial_fetch: bool,
) -> Option<Duration> {
    if state.cancel_token.is_cancelled() {
        trace!("Periodic fetcher was dropped. Stopping config reading");
        return None;
    }

    trace!(
        machine_id = %state.config.machine_id,
        "Fetching periodic configuration"
    );

    let accepted = state.netconf.load_full();
    let result = fetch(
        &state.config.machine_id,
        &state.config.forge_api,
        carbide_client_config,
        accepted.known().map(Arc::as_ref),
    )
    .await;
    // A successful RPC whose instance metadata fails to convert is still a
    // failed fetch, so finish conversion before choosing the retry delay.
    let result = match result {
        Ok(resp) => {
            state
                .netconf
                .store(Arc::new(NetworkConfigState::Known(Arc::new(resp.clone()))));
            instance_metadata_from_instance(resp.instance, state.sitename.clone())
        }
        Err(err) => Err(err),
    };
    // Startup consumers can need the cached config before a normal interval
    // passes. Preserve one immediate retry if the initial fetch fails.
    let next_fetch_delay = if initial_fetch && result.is_err() {
        Duration::ZERO
    } else {
        jittered_interval(state.config.config_fetch_interval, &mut rand::rng())
    };
    match result {
        Ok(config) => {
            state.instmeta.store(config.map(Arc::new));
            ConfigFetch::Succeeded.emit();
        }
        Err(err) => match err.downcast_ref::<tonic::Status>() {
            Some(grpc_status) if grpc_status.code() == tonic::Code::NotFound => {
                state.netconf.store(Arc::new(NetworkConfigState::Absent));
                state.instmeta.store(None);
                ConfigFetch::NotFound {
                    machine_id: state.config.machine_id.to_string(),
                }
                .emit();
            }
            _ => ConfigFetch::Failed {
                error: format!("{err:?}"),
                retry_interval_seconds: next_fetch_delay.as_secs_f64(),
            }
            .emit(),
        },
    }

    Some(next_fetch_delay)
}

/// Make the network request to get network config
async fn fetch(
    dpu_machine_id: &MachineId,
    forge_api: &str,
    client_config: &CarbideClientConfig,
    accepted: Option<&rpc::ManagedHostNetworkConfigResponse>,
) -> Result<rpc::ManagedHostNetworkConfigResponse, eyre::Report> {
    let mut client = create_forge_client(forge_api, client_config).await?;

    let mut response = get_periodic_dpu_config(&mut client, dpu_machine_id).await?;
    normalize_network_config_addresses(&mut response)?;
    validate_service_network_config(&response, accepted)?;
    Ok(response)
}

/// Validates fetched service networking so malformed topology can be rejected without erasing
/// the last usable service configuration.
///
/// Callers must run this validation before replacing the cached response.
fn validate_service_network_config(
    response: &rpc::ManagedHostNetworkConfigResponse,
    accepted: Option<&rpc::ManagedHostNetworkConfigResponse>,
) -> Result<(), eyre::Report> {
    if response.service_vpc_slot_inventory.is_none() {
        eyre::ensure!(
            response.service_interfaces.is_empty(),
            "service interfaces require a fixed-slot inventory"
        );

        // Accept legacy omission only while the last accepted response has no service endpoints or slots.
        if let Some(accepted) = accepted {
            let retained_service_state = !accepted.service_interfaces.is_empty()
                || accepted
                    .service_vpc_slot_inventory
                    .as_ref()
                    .is_some_and(|inventory| !inventory.slots.is_empty());
            eyre::ensure!(
                !retained_service_state,
                "fixed-slot inventory disappeared after service topology was accepted"
            );
        }
    }

    service_network::validate_and_build_ports(response).map(drop)
}

/// Validates the family-neutral address list and copies it into the compatibility fields used by
/// the agent.
fn normalize_network_config_addresses(
    response: &mut rpc::ManagedHostNetworkConfigResponse,
) -> Result<(), eyre::Report> {
    if let Some(interface) = &mut response.admin_interface {
        normalize_interface_addresses(interface).wrap_err_with(|| {
            format!(
                "invalid admin interface address configuration for VLAN {}",
                interface.vlan_id
            )
        })?;
    }

    for (index, interface) in response.tenant_interfaces.iter_mut().enumerate() {
        normalize_interface_addresses(interface).wrap_err_with(|| {
            format!(
                "invalid tenant interface address configuration at index {index} for VLAN {}",
                interface.vlan_id
            )
        })?;
    }

    Ok(())
}

/// Preserves a legacy payload, or validates a populated `addresses` list and copies it into legacy
/// fields.
#[allow(deprecated)]
fn normalize_interface_addresses(
    interface: &mut rpc::FlatInterfaceConfig,
) -> Result<(), eyre::Report> {
    if interface.addresses.is_empty() {
        return Ok(());
    }

    let legacy_tenant_vrf_loopback_ip = interface.tenant_vrf_loopback_ip.clone();
    let prefixless_legacy_ipv6 = interface
        .ipv6_interface_config
        .clone()
        .filter(|config| config.interface_prefix.is_empty());
    let mut ipv4 = None;
    let mut ipv6 = None;
    for address in &interface.addresses {
        let family = rpc::AddressFamily::try_from(address.address_family)
            .wrap_err_with(|| format!("unknown address family value {}", address.address_family))?;
        let declared_family = validate_interface_address(address, family)?;
        let slot = match declared_family {
            IpAddressFamily::Ipv4 => &mut ipv4,
            IpAddressFamily::Ipv6 => &mut ipv6,
        };

        if slot.replace(address.clone()).is_some() {
            return Err(eyre::eyre!("duplicate {family:?} address configuration"));
        }
    }

    let ipv4_loopback = ipv4
        .as_ref()
        .and_then(|address| address.tenant_vrf_loopback_ip.as_ref());
    let ipv6_loopback = ipv6
        .as_ref()
        .and_then(|address| address.tenant_vrf_loopback_ip.as_ref());
    // The deprecated compatibility scalar is IPv4-only. Writers without
    // InterfaceAddressConfig.tenant_vrf_loopback_ip populate only that scalar,
    // so preserve it when neither family entry mirrors the loopback. Keep an
    // authoritative IPv6 loopback only in the address list.
    interface.tenant_vrf_loopback_ip = match (ipv4_loopback, ipv6_loopback) {
        (Some(ipv4_loopback), None) => Some(ipv4_loopback.clone()),
        (None, Some(_)) => None,
        (None, None) => legacy_tenant_vrf_loopback_ip,
        (Some(_), Some(_)) => {
            return Err(eyre::eyre!(
                "tenant VRF loopback must be set on at most one address family"
            ));
        }
    };

    if let Some(ipv4) = ipv4 {
        interface.gateway.clone_from(&ipv4.gateway);
        interface.ip = (!ipv4.ip.is_empty()).then_some(ipv4.ip);
        interface.interface_prefix =
            (!ipv4.interface_prefix.is_empty()).then_some(ipv4.interface_prefix);
        interface.prefix = (!ipv4.prefix.is_empty()).then_some(ipv4.prefix);
        interface.svi_ip.clone_from(&ipv4.svi_ip);
    } else {
        interface.gateway = None;
        interface.ip = None;
        interface.interface_prefix = None;
        interface.prefix = None;
        interface.svi_ip = None;
    }
    interface.ipv6_interface_config = ipv6
        .as_ref()
        .filter(|address| {
            !address.ip.is_empty()
                || !address.interface_prefix.is_empty()
                || address.svi_ip.is_some()
        })
        .map(|address| rpc::FlatInterfaceIpv6Config {
            ip: address.ip.clone(),
            interface_prefix: address.interface_prefix.clone(),
            svi_ip: address.svi_ip.clone(),
        })
        // A writer that requires an IPv6 interface prefix can omit a prefixless
        // sidecar from its populated list, so preserve that compatibility value.
        .or(prefixless_legacy_ipv6);

    Ok(())
}

fn validate_interface_address(
    address: &rpc::InterfaceAddressConfig,
    family: rpc::AddressFamily,
) -> Result<IpAddressFamily, eyre::Report> {
    let declared_family = match family {
        rpc::AddressFamily::V4 => IpAddressFamily::Ipv4,
        rpc::AddressFamily::V6 => {
            if let Some(gateway) = address.gateway.as_deref() {
                return Err(eyre::eyre!(
                    "address configuration declared as V6 includes unsupported `gateway` value `{gateway}`"
                ));
            }
            IpAddressFamily::Ipv6
        }
        rpc::AddressFamily::Unspecified => {
            return Err(eyre::eyre!("address family must be V4 or V6"));
        }
    };

    for (field, value) in [
        (
            "ip",
            (!address.ip.is_empty()).then_some(address.ip.as_str()),
        ),
        (
            "tenant_vrf_loopback_ip",
            address.tenant_vrf_loopback_ip.as_deref(),
        ),
    ] {
        let Some(value) = value else {
            continue;
        };
        value
            .parse::<IpAddr>()
            .wrap_err_with(|| format!("invalid `{field}` value `{value}`"))?
            .require_address_family_or_else(declared_family, |_| {
                eyre::eyre!(
                    "value `{value}` for `{field}` does not match declared {family:?} family"
                )
            })?;
    }

    for (field, value) in [
        ("gateway", address.gateway.as_deref()),
        (
            "interface_prefix",
            (!address.interface_prefix.is_empty()).then_some(address.interface_prefix.as_str()),
        ),
        (
            "prefix",
            (!address.prefix.is_empty()).then_some(address.prefix.as_str()),
        ),
        ("svi_ip", address.svi_ip.as_deref()),
    ] {
        let Some(value) = value else {
            continue;
        };
        value
            .parse::<IpNetwork>()
            .wrap_err_with(|| format!("invalid `{field}` value `{value}`"))?
            .require_address_family_or_else(declared_family, |_| {
                eyre::eyre!(
                    "value `{value}` for `{field}` does not match declared {family:?} family"
                )
            })?;
    }

    Ok(declared_family)
}

/// Picks the lowest address in each family across physical interfaces because HostInband status
/// is built from maps and does not have a stable interface or address order.
fn select_public_addresses(
    interfaces: &[rpc::InstanceInterfaceStatus],
) -> Result<PublicAddresses, eyre::Error> {
    let mut public_addresses = PublicAddresses::default();

    let addresses = interfaces
        .iter()
        .filter(|interface| interface.virtual_function_id.is_none())
        .flat_map(|interface| &interface.addresses);

    for value in addresses {
        let address = value
            .parse::<IpAddr>()
            .wrap_err_with(|| format!("invalid physical interface address `{value}`"))?;

        match address {
            IpAddr::V4(address) => {
                public_addresses.ipv4 = Some(
                    public_addresses
                        .ipv4
                        .map_or(address, |current| current.min(address)),
                );
            }
            IpAddr::V6(address) => {
                public_addresses.ipv6 = Some(
                    public_addresses
                        .ipv6
                        .map_or(address, |current| current.min(address)),
                );
            }
        }
    }

    Ok(public_addresses)
}

fn instance_metadata_from_instance(
    instance: Option<Instance>,
    sitename: Option<String>,
) -> Result<Option<InstanceMetadata>, eyre::Error> {
    let instance = match instance {
        Some(instance) => instance,
        None => return Ok(None),
    };

    let hostname = match instance.id {
        Some(name) => name.to_string(),
        None => return Err(eyre::eyre!("host name is not present in tenant config")),
    };

    let machine_id = instance.machine_id;

    let instance_id = instance.id;

    let instance_name = instance
        .metadata
        .as_ref()
        .map(|metadata| metadata.name.clone())
        .filter(|name| !name.is_empty());

    let public_addresses = instance
        .status
        .as_ref()
        .and_then(|status| status.network.as_ref())
        .map(|network| select_public_addresses(&network.interfaces))
        .transpose()?
        .unwrap_or_default();

    let user_data = instance
        .config
        .as_ref()
        .and_then(|config| config.os.as_ref())
        .and_then(|os_config| os_config.user_data.clone())
        .unwrap_or_default();

    let devices = match extract_instance_ib_config(&instance) {
        Ok(value) => Some(value),
        Err(e) => {
            trace!(error = %e, "Failed to fetch IB config");
            None
        }
    };

    Ok(Some(InstanceMetadata {
        public_addresses,
        hostname,
        instance_name,
        sitename,
        instance_id,
        machine_id: machine_id.map(Into::into),
        user_data,
        ib_devices: devices,
        config_version: instance
            .config_version
            .parse()
            .wrap_err("failed to parse instance config_version")?,
        network_config_version: instance
            .network_config_version
            .parse()
            .wrap_err("failed to parse instance network_config_version")?,
        extension_service_version: instance
            .dpu_extension_service_version
            .parse()
            .wrap_err("failed to parse instance extension_service_version")?,
    }))
}

fn extract_instance_ib_config(instance: &Instance) -> Result<Vec<IBDeviceConfig>, eyre::Error> {
    let ib_config = instance
        .config
        .as_ref()
        .and_then(|config| config.infiniband.as_ref())
        .ok_or_else(|| eyre::eyre!("no infiniband interfaces found"))?;

    let ib_interface_configs = &ib_config.ib_interfaces;

    let ib_status = instance
        .status
        .as_ref()
        .and_then(|status| status.infiniband.as_ref())
        .ok_or_else(|| eyre::eyre!("no infiniband interfaces found"))?;

    let ib_interface_statuses = &ib_status.ib_interfaces;

    let mut devices: Vec<IBDeviceConfig> = Vec::new();

    for (index, config) in ib_interface_configs.iter().enumerate() {
        let status = &ib_interface_statuses[index];

        let instance: IBInstanceConfig = IBInstanceConfig {
            ib_partition_id: config.ib_partition_id,
            ib_guid: status.guid.clone(),
            lid: status.lid,
        };

        if let Some(pf_guid) = &status.pf_guid {
            match devices.iter_mut().find(|dev| &(dev.pf_guid) == pf_guid) {
                Some(device) => device.instances.push(instance),
                None => devices.push(IBDeviceConfig {
                    pf_guid: pf_guid.clone(),
                    instances: vec![instance],
                }),
            }
        } else {
            continue;
        }
    }

    if devices.is_empty() {
        return Err(eyre::eyre!("no infiniband devices found"));
    }

    Ok(devices)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    /// Builds one complete service endpoint so validation cases can change only the contract they
    /// exercise.
    fn service_interface(
        identity: u8,
        link_prefix: &str,
    ) -> rpc::ManagedHostServiceInterfaceConfig {
        rpc::ManagedHostServiceInterfaceConfig {
            service_id: format!("10000000-0000-0000-0000-{identity:012}"),
            attachment_id: Some(::rpc::common::Uuid {
                value: format!("20000000-0000-0000-0000-{identity:012}"),
            }),
            interface_ordinal: 0,
            slot_index: 0, // Both valid endpoints intentionally share one fixed port.
            vpc_id: Some("30000000-0000-0000-0000-000000000001".parse().unwrap()),
            vpc_vni: 10_001,
            vpc_prefixes: vec!["192.0.2.0/24".to_string(), "2001:db8::/64".to_string()],
            vpc_peer_prefixes: vec![],
            vpc_peer_vnis: vec![],
            vpc_routing_profile: Some(rpc::RoutingProfile::default()),
            interface_routing_profile: None,
            network_security_group: None,
            mtu: Some(9_000), // The fixed DPF and service-NAD contract must agree with HBN.
            link_prefix: link_prefix.to_string(),
            mac_address: format!("02:00:00:00:00:{identity:02x}"),
            internal_uuid: Some(::rpc::common::Uuid {
                value: format!("40000000-0000-0000-0000-{identity:012}"),
            }),
            tenant_vrf_loopback_ip: Some("198.51.100.10".to_string()),
        }
    }

    /// Builds a valid same-VPC, same-slot response with multiple IPv4 endpoints so validation cases
    /// exercise grouped service state independently of the host address model.
    fn service_network_config() -> rpc::ManagedHostNetworkConfigResponse {
        rpc::ManagedHostNetworkConfigResponse {
            network_virtualization_type: Some(rpc::VpcVirtualizationType::Fnn.into()),
            vpc_peer_vnis_authoritative: true,
            service_interfaces: vec![
                service_interface(1, "192.0.2.10/31"),
                service_interface(2, "192.0.2.12/31"),
            ],
            service_vpc_slot_inventory: Some(rpc::ServiceVpcSlotInventory {
                slots: vec![rpc::ServiceVpcSlot {
                    slot_index: 0,
                    hbn_interface_name: "iface_svc_0".to_string(),
                }],
            }),
            ..Default::default()
        }
    }

    /// Verifies service-network validation accepts supported legacy and grouped endpoint responses
    /// and rejects malformed service interfaces, slot inventory, and unrenderable NSGs so callers
    /// can retain the prior cached response.
    #[test]
    #[allow(deprecated)]
    fn validates_service_network_contract() {
        // Describe valid and rejected responses with the prior cache state relevant to omission.
        struct Case {
            name: &'static str,
            response: rpc::ManagedHostNetworkConfigResponse,
            accepted: Option<rpc::ManagedHostNetworkConfigResponse>,
            error: Option<&'static str>,
        }

        // Start from a complete response with two endpoints sharing one slot and vary one
        // validation rule per case.
        let valid = service_network_config();
        // Change only Core's projected virtualization type while retaining complete service topology.
        let mut etv_service_config = valid.clone();
        etv_service_config.network_virtualization_type =
            Some(rpc::VpcVirtualizationType::EthernetVirtualizer.into());
        // Represent explicit zero capacity with a present empty inventory.
        let present_zero = rpc::ManagedHostNetworkConfigResponse {
            service_vpc_slot_inventory: Some(rpc::ServiceVpcSlotInventory::default()),
            ..Default::default()
        };
        let legacy_host_only = rpc::ManagedHostNetworkConfigResponse::default();

        // Widen only the first IPv4 service link.
        let mut wrong_prefix_length = valid.clone();
        wrong_prefix_length.service_interfaces[0].link_prefix = "192.0.2.8/30".to_string();
        // Set the first /31 to its service-side address while retaining the correct prefix length.
        let mut noncanonical_prefix = valid.clone();
        noncanonical_prefix.service_interfaces[0].link_prefix = "192.0.2.11/31".to_string();
        let mut valid_ipv6 = valid.clone();
        // Keep one endpoint so this case isolates IPv6 link validation from an unrelated IPv4 member.
        valid_ipv6.service_interfaces.truncate(1);
        valid_ipv6.service_interfaces[0].link_prefix = "2001:db8::10/127".to_string();
        // Widen the IPv6 service link while retaining its canonical network address.
        let mut wrong_ipv6_prefix = valid_ipv6.clone();
        wrong_ipv6_prefix.service_interfaces[0].link_prefix = "2001:db8::10/126".to_string();
        // Set the /127 to its service-side address while retaining the correct prefix length.
        let mut noncanonical_ipv6_prefix = valid_ipv6.clone();
        noncanonical_ipv6_prefix.service_interfaces[0].link_prefix = "2001:db8::11/127".to_string();
        // Replace only the service identity with non-UUID text.
        let mut invalid_service_id = valid.clone();
        invalid_service_id.service_interfaces[0].service_id = "not-a-service-id".to_string();
        // Replace only the service identity with the nil UUID.
        let mut nil_service_id = valid.clone();
        nil_service_id.service_interfaces[0].service_id =
            "00000000-0000-0000-0000-000000000000".to_string();
        // Replace only the attachment identity with non-UUID text.
        let mut invalid_attachment_id = valid.clone();
        invalid_attachment_id.service_interfaces[0]
            .attachment_id
            .as_mut()
            .unwrap()
            .value = "not-a-uuid".to_string();
        // Replace only the internal reservation identity with the nil UUID.
        let mut nil_internal_uuid = valid.clone();
        nil_internal_uuid.service_interfaces[0]
            .internal_uuid
            .as_mut()
            .unwrap()
            .value = "00000000-0000-0000-0000-000000000000".to_string();
        // Replace only the first endpoint's MAC with non-MAC text.
        let mut invalid_mac = valid.clone();
        invalid_mac.service_interfaces[0].mac_address = "not-a-mac".to_string();
        // Reuse one MAC while retaining distinct attachment IDs, internal UUIDs and links.
        let mut duplicate_mac = valid.clone();
        duplicate_mac.service_interfaces[1].mac_address =
            valid.service_interfaces[0].mac_address.clone();
        // Replace only one selected-VPC prefix with non-prefix text.
        let mut invalid_vpc_prefix = valid.clone();
        invalid_vpc_prefix.service_interfaces[0].vpc_prefixes[0] = "not-a-prefix".to_string();
        // Attach an instance-sourced NSG to the first service endpoint.
        let mut non_vpc_nsg = valid.clone();
        non_vpc_nsg.service_interfaces[0].network_security_group =
            Some(rpc::FlatInterfaceNetworkSecurityGroupConfig {
                id: "service-nsg".to_string(),
                source: rpc::NetworkSecurityGroupSource::NsgSourceInstance.into(),
                ..Default::default()
            });
        // Keep both grouped endpoints aligned so malformed rule conversion is the only failure.
        let mut malformed_nsg_rule = valid.clone();
        for interface in &mut malformed_nsg_rule.service_interfaces {
            interface.network_security_group = Some(rpc::FlatInterfaceNetworkSecurityGroupConfig {
                id: "service-nsg".to_string(),
                source: rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(),
                rules: vec![rpc::ResolvedNetworkSecurityGroupRule::default()],
                ..Default::default()
            });
        }
        // Split exactly 10,000 expanded rules across two rules to exercise per-NSG accumulation.
        let mut maximum_nsg = valid.clone();
        // Keep one endpoint to isolate NSG capacity from grouped-policy agreement.
        maximum_nsg.service_interfaces.truncate(1);
        maximum_nsg.service_interfaces[0].network_security_group =
            Some(rpc::FlatInterfaceNetworkSecurityGroupConfig {
                id: "service-nsg".to_string(),
                source: rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(),
                rules: [("first-range", 1, 5_000), ("second-range", 5_001, 10_000)]
                    .into_iter()
                    .map(|(id, start, end)| rpc::ResolvedNetworkSecurityGroupRule {
                        rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
                            id: Some(id.to_string()),
                            direction:
                                rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                                    .into(),
                            protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                            action: rpc::NetworkSecurityGroupRuleAction::NsgRuleActionPermit.into(),
                            dst_port_start: Some(start), // Each range stays individually below the limit.
                            dst_port_end: Some(end),
                            ..Default::default()
                        }),
                        src_prefixes: vec!["0.0.0.0/0".to_string()],
                        dst_prefixes: vec!["0.0.0.0/0".to_string()],
                    })
                    .collect(),
                ..Default::default()
            });
        // Add one expansion to the second rule while keeping both individual counts below 10,000.
        let mut oversized_nsg = maximum_nsg.clone();
        oversized_nsg.service_interfaces[0]
            .network_security_group
            .as_mut()
            .expect("the service endpoint has an NSG")
            .rules[1]
            .rule
            .as_mut()
            .expect("the resolved NSG rule has attributes")
            .dst_port_end = Some(10_001);
        // Keep both grouped endpoints aligned so unresolved route targets are the only failure.
        let mut malformed_routing_profile = valid.clone();
        for interface in &mut malformed_routing_profile.service_interfaces {
            interface
                .vpc_routing_profile
                .as_mut()
                .unwrap()
                .route_target_imports
                .push(::rpc::common::RouteTarget {
                    asn: 65_000,
                    vni: None,
                    vni_self: true,
                });
        }
        // Replace only the first endpoint's fixed service MTU.
        let mut wrong_mtu = valid.clone();
        wrong_mtu.service_interfaces[0].mtu = Some(1_500);
        // Move only the first point-to-point link outside its selected VPC prefixes.
        let mut link_outside_vpc = valid.clone();
        link_outside_vpc.service_interfaces[0].link_prefix = "198.51.100.10/31".to_string();
        // Reuse the first link on the independently identified second endpoint.
        let mut duplicate_link = valid.clone();
        duplicate_link.service_interfaces[1].link_prefix =
            valid.service_interfaces[0].link_prefix.clone();
        let mut without_loopback = valid.clone();
        // Clear every grouped endpoint because `use_vpc_vrf_loopback` applies site-wide.
        for interface in &mut without_loopback.service_interfaces {
            interface.tenant_vrf_loopback_ip = None;
        }
        // Replace only the first endpoint's optional loopback with non-address text.
        let mut invalid_loopback = valid.clone();
        invalid_loopback.service_interfaces[0].tenant_vrf_loopback_ip =
            Some("not-an-ipv4-address".to_string());
        // Append slot two while leaving the expected slot-one entry absent.
        let mut noncontiguous_inventory = valid.clone();
        noncontiguous_inventory
            .service_vpc_slot_inventory
            .as_mut()
            .unwrap()
            .slots
            .push(rpc::ServiceVpcSlot {
                slot_index: 2,
                hbn_interface_name: "iface_svc_2".to_string(),
            });
        // Clear only the configured HBN interface name.
        let mut empty_inventory_name = valid.clone();
        empty_inventory_name
            .service_vpc_slot_inventory
            .as_mut()
            .unwrap()
            .slots[0]
            .hbn_interface_name
            .clear();
        // Move the first endpoint to a slot absent from the inventory.
        let mut unconfigured_slot = valid.clone();
        unconfigured_slot.service_interfaces[0].slot_index = 1;
        let mut slot_conflict = valid.clone();
        // Change both VPC fields so this response isolates one slot being bound to two VPCs.
        slot_conflict.service_interfaces[1].vpc_id =
            Some("30000000-0000-0000-0000-000000000002".parse().unwrap());
        slot_conflict.service_interfaces[1].vpc_vni = 10_002;
        let mut vpc_split_across_slots = valid.clone();
        // Add a valid second slot before moving one endpoint to isolate a VPC split across slots.
        vpc_split_across_slots
            .service_vpc_slot_inventory
            .as_mut()
            .unwrap()
            .slots
            .push(rpc::ServiceVpcSlot {
                slot_index: 1,
                hbn_interface_name: "iface_svc_1".to_string(),
            });
        vpc_split_across_slots.service_interfaces[1].slot_index = 1;
        let mut vni_shared_across_vpcs = valid.clone();
        // Give the second VPC its own slot while retaining the first VPC's VNI.
        vni_shared_across_vpcs
            .service_vpc_slot_inventory
            .as_mut()
            .unwrap()
            .slots
            .push(rpc::ServiceVpcSlot {
                slot_index: 1,
                hbn_interface_name: "iface_svc_1".to_string(),
            });
        vni_shared_across_vpcs.service_interfaces[1].slot_index = 1;
        vni_shared_across_vpcs.service_interfaces[1].vpc_id =
            Some("30000000-0000-0000-0000-000000000002".parse().unwrap());
        // Change only one peer-prefix collection within the shared slot.
        let mut inconsistent_group_policy = valid.clone();
        inconsistent_group_policy.service_interfaces[1]
            .vpc_peer_prefixes
            .push("203.0.113.0/24".to_string());
        // Mirror the selected VPC fields needed for a valid shared host/service VNI.
        let shared_host = rpc::FlatInterfaceConfig {
            vpc_vni: valid.service_interfaces[0].vpc_vni,
            vpc_prefixes: valid.service_interfaces[0].vpc_prefixes.clone(),
            vpc_peer_prefixes: valid.service_interfaces[0].vpc_peer_prefixes.clone(),
            vpc_peer_vnis: valid.service_interfaces[0].vpc_peer_vnis.clone(),
            vpc_routing_profile: valid.service_interfaces[0].vpc_routing_profile.clone(),
            tenant_vrf_loopback_ip: valid.service_interfaces[0].tenant_vrf_loopback_ip.clone(),
            ..Default::default()
        };
        let mut inconsistent_host_routing = valid.clone();
        // Change only the host routing profile before sharing the service VNI.
        let mut host_with_different_routing = shared_host.clone();
        host_with_different_routing
            .vpc_routing_profile
            .as_mut()
            .unwrap()
            .leak_default_route_from_underlay = true;
        inconsistent_host_routing.tenant_interfaces = vec![host_with_different_routing];
        let mut inconsistent_host_loopback = valid.clone();
        // Change only the host loopback before sharing the service VNI.
        let mut host_with_different_loopback = shared_host.clone();
        host_with_different_loopback.tenant_vrf_loopback_ip = Some("198.51.100.11".to_string());
        inconsistent_host_loopback.tenant_interfaces = vec![host_with_different_loopback];
        // Keep IPv4 service links while selecting an IPv6 loopback for every grouped endpoint.
        let mut ipv6_loopback = valid.clone();
        for interface in &mut ipv6_loopback.service_interfaces {
            interface.tenant_vrf_loopback_ip = Some("2001:db8::3".to_string());
        }
        // Use an equivalent IPv6 spelling to compare addresses rather than string representations.
        let mut shared_ipv6_loopback = ipv6_loopback.clone();
        shared_ipv6_loopback.tenant_interfaces = vec![rpc::FlatInterfaceConfig {
            tenant_vrf_loopback_ip: None, // IPv6 is carried only in the family-neutral entry.
            addresses: vec![rpc::InterfaceAddressConfig {
                address_family: rpc::AddressFamily::V6.into(),
                tenant_vrf_loopback_ip: Some("2001:0db8:0:0:0:0:0:3".to_string()),
                ..Default::default()
            }],
            ..shared_host
        }];
        // Change only the canonical host loopback to retain the shared-VRF conflict guard for IPv6.
        let mut conflicting_ipv6_loopback = shared_ipv6_loopback.clone();
        conflicting_ipv6_loopback.tenant_interfaces[0].addresses[0].tenant_vrf_loopback_ip =
            Some("2001:db8::4".to_string());
        // Append a second slot that reuses the first slot's HBN interface name.
        let mut duplicate_inventory_name = valid.clone();
        duplicate_inventory_name
            .service_vpc_slot_inventory
            .as_mut()
            .unwrap()
            .slots
            .push(rpc::ServiceVpcSlot {
                slot_index: 1,
                hbn_interface_name: "iface_svc_0".to_string(),
            });
        // Rename the service slot to the representor used by the host fixture below.
        let mut host_name_collision = valid.clone();
        host_name_collision
            .service_vpc_slot_inventory
            .as_mut()
            .unwrap()
            .slots[0]
            .hbn_interface_name = "pf0vf0_if".to_string();
        // Add one VF solely to make `pf0vf0_if` an occupied host port name.
        host_name_collision.tenant_interfaces = vec![rpc::FlatInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Virtual.into(),
            virtual_function_id: Some(0),
            vpc_vni: 99_999,
            ..Default::default()
        }];
        // Remove inventory while retaining both service endpoints.
        let mut missing_inventory = valid.clone();
        missing_inventory.service_vpc_slot_inventory = None;
        // Add a site-global VNI while retaining the fixed service inventory.
        let mut site_global = valid.clone();
        site_global.site_global_vpc_vni = Some(20_000);
        // Remove service interfaces while retaining nonempty inventory so omission checks cannot
        // rely on previously accepted endpoints.
        let mut accepted_inventory_only = valid.clone();
        accepted_inventory_only.service_interfaces.clear();

        let cases = vec![
            // Legacy host-only responses remain acceptable during an agent-first rolling upgrade.
            Case {
                name: "initial legacy omission",
                response: legacy_host_only.clone(),
                accepted: None,
                error: None,
            },
            // Presence-bearing zero capacity is valid and remains distinct from an omission.
            Case {
                name: "present zero-slot inventory",
                response: present_zero,
                accepted: None,
                error: None,
            },
            // Same-family endpoints share one slot without entering the one-address-per-family host model.
            Case {
                name: "multiple IPv4 endpoints on one slot",
                response: valid.clone(),
                accepted: None,
                error: None,
            },
            // Fetched ETV responses cannot activate service endpoints requiring routed FNN ports.
            Case {
                name: "non-FNN service response",
                response: etv_service_config,
                accepted: None,
                error: Some("service interfaces require FNN virtualization"),
            },
            // Canonical IPv6 links retain the independently staged /127 HBN contract.
            Case {
                name: "canonical IPv6 endpoint",
                response: valid_ipv6,
                accepted: None,
                error: None,
            },
            // Malformed service identity must not enter the accepted cache as an opaque string.
            Case {
                name: "invalid service identity",
                response: invalid_service_id,
                accepted: None,
                error: Some("invalid service_id"),
            },
            // A nil service identity cannot provide stable ownership across endpoint replacement.
            Case {
                name: "nil service identity",
                response: nil_service_id,
                accepted: None,
                error: Some("service_id must not be nil"),
            },
            // Malformed wire UUID text must fail before DHCP reservation identity is constructed.
            Case {
                name: "invalid attachment identity",
                response: invalid_attachment_id,
                accepted: None,
                error: Some("invalid service attachment_id"),
            },
            // A nil internal UUID cannot identify the target of a DHCP reservation.
            Case {
                name: "nil internal UUID",
                response: nil_internal_uuid,
                accepted: None,
                error: Some("service internal_uuid must not be nil"),
            },
            // A syntactically invalid MAC cannot become an HBN or DHCP endpoint.
            Case {
                name: "invalid service MAC",
                response: invalid_mac,
                accepted: None,
                error: Some("invalid service MAC"),
            },
            // Otherwise valid endpoints on one slot cannot create an ambiguous DHCP MAC lookup.
            Case {
                name: "duplicate service MAC on one slot",
                response: duplicate_mac,
                accepted: None,
                error: Some("duplicate MAC"),
            },
            // A malformed selected-VPC prefix cannot safely bound the endpoint link.
            Case {
                name: "invalid VPC prefix",
                response: invalid_vpc_prefix,
                accepted: None,
                error: Some("invalid VPC prefix"),
            },
            // Service policy must come from the selected VPC rather than a consumer instance.
            Case {
                name: "instance-sourced service NSG",
                response: non_vpc_nsg,
                accepted: None,
                error: Some("must come from the selected VPC"),
            },
            // A missing resolved rule cannot become accepted service ACL state.
            Case {
                name: "malformed service NSG rule",
                response: malformed_nsg_rule,
                accepted: None,
                error: Some("attempting to convert empty NSG rule"),
            },
            // Reaching the renderer's cumulative limit must remain accepted for compatible policies.
            Case {
                name: "service NSG at expanded-rule limit",
                response: maximum_nsg,
                accepted: None,
                error: None,
            },
            // Individually small rules cannot exceed the per-NSG limit before cache publication.
            Case {
                name: "service NSG exceeds cumulative expanded-rule limit",
                response: oversized_nsg,
                accepted: None,
                error: Some("supplied network security group rule count exceeds limit of 10000"),
            },
            // A Core-owned symbolic route target must be resolved before it reaches the DPU agent.
            Case {
                name: "unresolved service route target",
                response: malformed_routing_profile,
                accepted: None,
                error: Some("received unresolved route target with vni_self set"),
            },
            // A wider IPv4 network does not provide the fixed two-address point-to-point contract.
            Case {
                name: "wrong IPv4 prefix length",
                response: wrong_prefix_length,
                accepted: None,
                error: Some("canonical /31"),
            },
            // Host bits in the supplied prefix would reverse the fixed base+0/base+1 ownership.
            Case {
                name: "noncanonical IPv4 prefix",
                response: noncanonical_prefix,
                accepted: None,
                error: Some("canonical /31"),
            },
            // A wider IPv6 network cannot identify the fixed HBN and service peers.
            Case {
                name: "wrong IPv6 prefix length",
                response: wrong_ipv6_prefix,
                accepted: None,
                error: Some("canonical /127"),
            },
            // Supplying the service-side address as the /127 base would reverse fixed peer ownership.
            Case {
                name: "noncanonical IPv6 prefix",
                response: noncanonical_ipv6_prefix,
                accepted: None,
                error: Some("canonical /127"),
            },
            // HBN and the fixed DPF service topology must agree on the endpoint MTU.
            Case {
                name: "wrong service MTU",
                response: wrong_mtu,
                accepted: None,
                error: Some("MTU must be 9000"),
            },
            // A valid point-to-point prefix still cannot escape the selected VPC's address space.
            Case {
                name: "service link outside selected VPC",
                response: link_outside_vpc,
                accepted: None,
                error: Some("outside the selected VPC prefixes"),
            },
            // Independent reservation identities cannot share one point-to-point service link.
            Case {
                name: "duplicate service link",
                response: duplicate_link,
                accepted: None,
                error: Some("duplicate service link"),
            },
            // `use_vpc_vrf_loopback = false` omits this field without invalidating service endpoints.
            Case {
                name: "optional tenant VRF loopback omitted",
                response: without_loopback,
                accepted: None,
                error: None,
            },
            // The loopback family is independent of the service link's IPv4 family.
            Case {
                name: "IPv6 tenant VRF loopback",
                response: ipv6_loopback,
                accepted: None,
                error: None,
            },
            // Equal host/service IPv6 addresses agree even when their text representations differ.
            Case {
                name: "shared canonical host and service IPv6 loopback",
                response: shared_ipv6_loopback,
                accepted: None,
                error: None,
            },
            // A host loopback stored only in addresses must still participate in shared-VRF validation.
            Case {
                name: "shared host and service IPv6 loopback disagreement",
                response: conflicting_ipv6_loopback,
                accepted: None,
                error: Some("different loopbacks"),
            },
            // A present loopback remains syntax checked before it can reach the HBN template.
            Case {
                name: "invalid tenant VRF loopback",
                response: invalid_loopback,
                accepted: None,
                error: Some("invalid tenant VRF loopback"),
            },
            // Gapped slot indices cannot select the HBN and DHCP receiving interface by inventory
            // vector position.
            Case {
                name: "noncontiguous slot inventory",
                response: noncontiguous_inventory,
                accepted: None,
                error: Some("not contiguous"),
            },
            // Empty names cannot select the provisioned HBN and DHCP receiving interface.
            Case {
                name: "empty inventory name",
                response: empty_inventory_name,
                accepted: None,
                error: Some("empty HBN interface name"),
            },
            // An endpoint cannot invent a slot outside the complete fixed inventory.
            Case {
                name: "endpoint outside inventory",
                response: unconfigured_slot,
                accepted: None,
                error: Some("unconfigured slot"),
            },
            // One physical slot cannot join two VPC VRFs at once.
            Case {
                name: "one slot mapped to two VPCs",
                response: slot_conflict,
                accepted: None,
                error: Some("bound to both VPC"),
            },
            // One local VPC must aggregate on one slot rather than split its hidden port state.
            Case {
                name: "one VPC mapped to two slots",
                response: vpc_split_across_slots,
                accepted: None,
                error: Some("bound to both slot"),
            },
            // One VNI cannot identify two distinct VPCs even when each VPC has its own slot.
            Case {
                name: "one VNI mapped to two VPCs",
                response: vni_shared_across_vpcs,
                accepted: None,
                error: Some("associated with both VPC"),
            },
            // Endpoints sharing one slot must agree on every selected-VPC and port policy field.
            Case {
                name: "grouped endpoint policy disagreement",
                response: inconsistent_group_policy,
                accepted: None,
                error: Some("inconsistent VPC or port policy"),
            },
            // Host and service ports sharing a VNI cannot render conflicting VPC routing state.
            Case {
                name: "shared host and service routing disagreement",
                response: inconsistent_host_routing,
                accepted: None,
                error: Some("inconsistent state for VNI"),
            },
            // Two supplied loopbacks for one VNI must identify the same VPC VRF address.
            Case {
                name: "shared host and service loopback disagreement",
                response: inconsistent_host_loopback,
                accepted: None,
                error: Some("different loopbacks"),
            },
            // Duplicate HBN interface names would make two slots share one HBN port and DHCP
            // receiving interface.
            Case {
                name: "duplicate inventory name",
                response: duplicate_inventory_name,
                accepted: None,
                error: Some("duplicate service slot HBN interface name"),
            },
            // A service port must not overwrite a normal host representor in generated NVUE state.
            Case {
                name: "host port name collision",
                response: host_name_collision,
                accepted: None,
                error: Some("collides with a host port"),
            },
            // Endpoints are never interpreted as removed merely because a later response omits inventory.
            Case {
                name: "endpoint response without inventory",
                response: missing_inventory,
                accepted: Some(valid),
                error: Some("require a fixed-slot inventory"),
            },
            // Reject omission after a nonempty slot inventory was accepted without endpoints so
            // callers can retain the cached fixed-slot inventory.
            Case {
                name: "inventory disappears after accepted service state",
                response: legacy_host_only,
                accepted: Some(accepted_inventory_only),
                error: Some("inventory disappeared"),
            },
            // Site-global VNI mode would collapse service VPC isolation into one shared VRF.
            Case {
                name: "site-global VNI with service slots",
                response: site_global,
                accepted: None,
                error: Some("incompatible with site_global_vpc_vni"),
            },
        ];

        for case in cases {
            // Run the service-network validation used by fetch against each case's last accepted
            // response.
            let result = validate_service_network_config(&case.response, case.accepted.as_ref());

            // Each rejection identifies the violated contract, while valid compatibility forms succeed.
            match case.error {
                Some(expected) => assert!(
                    format!("{:#}", result.expect_err(case.name)).contains(expected),
                    "{} did not report {expected}",
                    case.name
                ),
                None => result.expect(case.name),
            }
        }
    }

    /// A DPU only blanks its networking on evidence that it was deleted. An
    /// unanswered fetch is not that evidence for a DPU that retains its HBN
    /// configuration, and the flag never suppresses a `NotFound`.
    #[test]
    fn reset_follows_the_answer_core_gave_not_the_absence_of_one() {
        #[derive(Debug)]
        struct Row {
            state: NetworkConfigState,
            retain_hbn_config: bool,
        }

        value_scenarios!(run = |row: Row| row.state.requires_reset(row.retain_hbn_config);
            "a configuration is applied, not reset" {
                Row {
                    state: NetworkConfigState::Known(Arc::default()),
                    retain_hbn_config: false,
                } => false,
            }

            "a designated machine with a configuration is not reset either" {
                Row {
                    state: NetworkConfigState::Known(Arc::default()),
                    retain_hbn_config: true,
                } => false,
            }

            "NotFound deletes an undesignated machine" {
                Row {
                    state: NetworkConfigState::Absent,
                    retain_hbn_config: false,
                } => true,
            }

            "NotFound deletes a designated machine too" {
                Row {
                    state: NetworkConfigState::Absent,
                    retain_hbn_config: true,
                } => true,
            }

            "an unanswered fetch still resets an undesignated machine" {
                Row {
                    state: NetworkConfigState::Unknown,
                    retain_hbn_config: false,
                } => true,
            }

            "an unanswered fetch leaves a designated machine alone" {
                Row {
                    state: NetworkConfigState::Unknown,
                    retain_hbn_config: true,
                } => false,
            }
        );
    }

    // Builds the deprecated compatibility shape used by older Core versions.
    #[allow(deprecated)]
    fn legacy_interface(addresses: Vec<rpc::InterfaceAddressConfig>) -> rpc::FlatInterfaceConfig {
        rpc::FlatInterfaceConfig {
            vlan_id: 100,
            gateway: Some("198.51.100.1/24".to_string()),
            ip: Some("198.51.100.10".to_string()),
            interface_prefix: Some("198.51.100.10/32".to_string()),
            prefix: Some("198.51.100.0/24".to_string()),
            svi_ip: Some("198.51.100.2/24".to_string()),
            ipv6_interface_config: Some(rpc::FlatInterfaceIpv6Config {
                ip: "2001:db8:ffff::10".to_string(),
                interface_prefix: "2001:db8:ffff::10/128".to_string(),
                svi_ip: Some("2001:db8:ffff::2/64".to_string()),
            }),
            addresses,
            ..Default::default()
        }
    }

    fn ipv4_address() -> rpc::InterfaceAddressConfig {
        rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V4.into(),
            ip: "192.0.2.10".to_string(),
            interface_prefix: "192.0.2.10/32".to_string(),
            prefix: "192.0.2.0/24".to_string(),
            gateway: Some("192.0.2.1/24".to_string()),
            svi_ip: Some("192.0.2.2/24".to_string()),
            tenant_vrf_loopback_ip: None,
        }
    }

    fn ipv6_address() -> rpc::InterfaceAddressConfig {
        rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V6.into(),
            ip: "2001:db8::1".to_string(),
            interface_prefix: "2001:db8::1/128".to_string(),
            prefix: "2001:db8::/64".to_string(),
            gateway: None,
            svi_ip: Some("2001:db8::2/64".to_string()),
            tenant_vrf_loopback_ip: None,
        }
    }

    fn normalized_interface(
        mut interface: rpc::FlatInterfaceConfig,
    ) -> Result<rpc::FlatInterfaceConfig, ()> {
        normalize_interface_addresses(&mut interface)
            .map(|()| interface)
            .map_err(drop)
    }

    // Describes the compatibility projection consumed by the current agent.
    #[allow(deprecated)]
    fn expected_ipv4_interface() -> rpc::FlatInterfaceConfig {
        rpc::FlatInterfaceConfig {
            vlan_id: 100,
            gateway: Some("192.0.2.1/24".to_string()),
            ip: Some("192.0.2.10".to_string()),
            interface_prefix: Some("192.0.2.10/32".to_string()),
            prefix: Some("192.0.2.0/24".to_string()),
            svi_ip: Some("192.0.2.2/24".to_string()),
            ipv6_interface_config: None,
            addresses: vec![ipv4_address()],
            ..Default::default()
        }
    }

    // Describes an authoritative IPv6-only projection with cleared IPv4 compatibility fields.
    #[allow(deprecated)]
    fn expected_ipv6_interface() -> rpc::FlatInterfaceConfig {
        rpc::FlatInterfaceConfig {
            vlan_id: 100,
            ipv6_interface_config: Some(rpc::FlatInterfaceIpv6Config {
                ip: "2001:db8::1".to_string(),
                interface_prefix: "2001:db8::1/128".to_string(),
                svi_ip: Some("2001:db8::2/64".to_string()),
            }),
            addresses: vec![ipv6_address()],
            ..Default::default()
        }
    }

    // Exercises authoritative clearing of the deprecated optional SVI field.
    #[test]
    #[allow(deprecated)]
    fn family_neutral_addresses_replace_or_fall_back_to_legacy_fields() {
        let legacy = legacy_interface(vec![]);
        let mut dual_stack = expected_ipv4_interface();
        dual_stack.ipv6_interface_config = Some(rpc::FlatInterfaceIpv6Config {
            ip: "2001:db8::1".to_string(),
            interface_prefix: "2001:db8::1/128".to_string(),
            svi_ip: Some("2001:db8::2/64".to_string()),
        });
        dual_stack.addresses = vec![ipv6_address(), ipv4_address()];

        let mut ipv4_without_svi = ipv4_address();
        ipv4_without_svi.svi_ip = None;
        let mut expected_ipv4_without_svi = expected_ipv4_interface();
        expected_ipv4_without_svi.svi_ip = None;
        expected_ipv4_without_svi.addresses = vec![ipv4_without_svi.clone()];

        let mut ipv6_without_svi = ipv6_address();
        ipv6_without_svi.svi_ip = None;
        let mut dual_stack_without_ipv6_svi = expected_ipv4_interface();
        dual_stack_without_ipv6_svi.ipv6_interface_config = Some(rpc::FlatInterfaceIpv6Config {
            ip: "2001:db8::1".to_string(),
            interface_prefix: "2001:db8::1/128".to_string(),
            svi_ip: None,
        });
        dual_stack_without_ipv6_svi.addresses = vec![ipv4_address(), ipv6_without_svi.clone()];

        let mut prefixless_ipv6 = ipv6_address();
        prefixless_ipv6.interface_prefix.clear();
        let mut expected_with_prefixless_ipv6 = expected_ipv4_interface();
        expected_with_prefixless_ipv6.ipv6_interface_config = Some(rpc::FlatInterfaceIpv6Config {
            ip: "2001:db8::1".to_string(),
            interface_prefix: String::new(),
            svi_ip: Some("2001:db8::2/64".to_string()),
        });
        expected_with_prefixless_ipv6.addresses = vec![ipv4_address(), prefixless_ipv6.clone()];

        let mut slaac_ipv6 = ipv6_address();
        slaac_ipv6.ip.clear();
        let mut expected_slaac_ipv6 = expected_ipv6_interface();
        expected_slaac_ipv6
            .ipv6_interface_config
            .as_mut()
            .expect("IPv6 test fixture must include its compatibility fields")
            .ip
            .clear();
        expected_slaac_ipv6.addresses = vec![slaac_ipv6.clone()];

        let ipv4_loopback = rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V4.into(),
            tenant_vrf_loopback_ip: Some("192.0.2.3".to_string()),
            ..Default::default()
        };
        let mut expected_ipv6_with_ipv4_loopback = expected_ipv6_interface();
        expected_ipv6_with_ipv4_loopback.tenant_vrf_loopback_ip = Some("192.0.2.3".to_string());
        expected_ipv6_with_ipv4_loopback.addresses = vec![ipv4_loopback.clone(), ipv6_address()];

        let ipv6_loopback = rpc::InterfaceAddressConfig {
            address_family: rpc::AddressFamily::V6.into(),
            tenant_vrf_loopback_ip: Some("2001:db8::3".to_string()),
            ..Default::default()
        };
        let mut ipv4_with_ipv6_loopback =
            legacy_interface(vec![ipv4_address(), ipv6_loopback.clone()]);
        ipv4_with_ipv6_loopback.tenant_vrf_loopback_ip = Some("192.0.2.3".to_string());
        let mut expected_ipv4_with_ipv6_loopback = expected_ipv4_interface();
        expected_ipv4_with_ipv6_loopback.addresses = vec![ipv4_address(), ipv6_loopback];

        let mut partially_mirrored_legacy = legacy_interface(vec![ipv4_address()]);
        partially_mirrored_legacy.tenant_vrf_loopback_ip = Some("192.0.2.3".to_string());
        partially_mirrored_legacy.ipv6_interface_config = Some(rpc::FlatInterfaceIpv6Config {
            ip: "2001:db8::1".to_string(),
            interface_prefix: String::new(),
            svi_ip: Some("2001:db8::2/64".to_string()),
        });
        let mut expected_partially_mirrored_legacy = expected_ipv4_interface();
        expected_partially_mirrored_legacy.tenant_vrf_loopback_ip = Some("192.0.2.3".to_string());
        expected_partially_mirrored_legacy.ipv6_interface_config =
            partially_mirrored_legacy.ipv6_interface_config.clone();

        scenarios!(run = normalized_interface;
            "empty list falls back to legacy fields" {
                legacy.clone() => Yields(legacy),
            }
            "IPv4 list overrides legacy fields and clears stale IPv6" {
                legacy_interface(vec![ipv4_address()]) => Yields(expected_ipv4_interface()),
            }
            "reversed dual-stack list is selected by family without reordering" {
                legacy_interface(vec![ipv6_address(), ipv4_address()]) => Yields(dual_stack),
            }
            "IPv6-only list clears stale IPv4 compatibility fields" {
                legacy_interface(vec![ipv6_address()]) => Yields(expected_ipv6_interface()),
            }
            "absent V4 SVI clears the legacy value" {
                legacy_interface(vec![ipv4_without_svi]) => Yields(expected_ipv4_without_svi),
            }
            "absent V6 SVI clears the legacy sidecar value" {
                legacy_interface(vec![ipv4_address(), ipv6_without_svi]) => Yields(dual_stack_without_ipv6_svi),
            }
            "prefixless V6 address reconstructs the legacy sidecar" {
                legacy_interface(vec![ipv4_address(), prefixless_ipv6]) => Yields(expected_with_prefixless_ipv6),
            }
            "empty SLAAC host address remains empty" {
                legacy_interface(vec![slaac_ipv6]) => Yields(expected_slaac_ipv6),
            }
            "IPv4 loopback-only and IPv6 interface entries project by family" {
                legacy_interface(vec![ipv4_loopback, ipv6_address()]) => Yields(expected_ipv6_with_ipv4_loopback),
            }
            "IPv6 loopback stays in the list and clears the IPv4-only legacy scalar" {
                ipv4_with_ipv6_loopback => Yields(expected_ipv4_with_ipv6_loopback),
            }
            "partially mirrored legacy values survive a populated list" {
                partially_mirrored_legacy => Yields(expected_partially_mirrored_legacy),
            }
        );
    }

    #[test]
    fn family_neutral_addresses_reject_invalid_family_sets() {
        let mut unspecified = ipv4_address();
        unspecified.address_family = rpc::AddressFamily::Unspecified.into();
        let mut unknown = ipv4_address();
        unknown.address_family = 99;
        let mut ipv4_loopback = ipv4_address();
        ipv4_loopback.tenant_vrf_loopback_ip = Some("192.0.2.3".to_string());
        let mut ipv6_loopback = ipv6_address();
        ipv6_loopback.tenant_vrf_loopback_ip = Some("2001:db8::3".to_string());

        scenarios!(run = normalized_interface;
            "explicit family is required" {
                legacy_interface(vec![unspecified]) => Fails,
            }
            "unknown family is rejected" {
                legacy_interface(vec![unknown]) => Fails,
            }
            "duplicate V4 is rejected" {
                legacy_interface(vec![ipv4_address(), ipv4_address()]) => Fails,
            }
            "duplicate V6 is rejected" {
                legacy_interface(vec![ipv4_address(), ipv6_address(), ipv6_address()]) => Fails,
            }
            "loopback on both families is rejected" {
                legacy_interface(vec![ipv4_loopback, ipv6_loopback]) => Fails,
            }
        );
    }

    #[test]
    fn family_neutral_addresses_reject_invalid_values() {
        struct Case {
            scenario: &'static str,
            address: rpc::InterfaceAddressConfig,
            expected_messages: &'static [&'static str],
        }

        let mut ipv4_gateway = ipv4_address();
        ipv4_gateway.gateway = Some("2001:db8::1/64".to_string());
        let mut ipv6_ip = ipv6_address();
        ipv6_ip.ip = "192.0.2.10".to_string();
        let mut ipv4_interface_prefix = ipv4_address();
        ipv4_interface_prefix.interface_prefix = "2001:db8::10/128".to_string();
        let mut ipv4_prefix = ipv4_address();
        ipv4_prefix.prefix = "2001:db8::/64".to_string();
        let mut ipv4_svi = ipv4_address();
        ipv4_svi.svi_ip = Some("2001:db8::2/64".to_string());
        let mut ipv4_loopback = ipv4_address();
        ipv4_loopback.tenant_vrf_loopback_ip = Some("2001:db8::3".to_string());
        let mut malformed_ip = ipv4_address();
        malformed_ip.ip = "not-an-ip".to_string();
        let mut malformed_prefix = ipv4_address();
        malformed_prefix.prefix = "not-a-prefix".to_string();
        let mut empty_ipv4_gateway = ipv4_address();
        empty_ipv4_gateway.gateway = Some(String::new());
        let mut empty_svi = ipv4_address();
        empty_svi.svi_ip = Some(String::new());
        let mut empty_loopback = ipv4_address();
        empty_loopback.tenant_vrf_loopback_ip = Some(String::new());
        let mut ipv6_gateway = ipv6_address();
        ipv6_gateway.gateway = Some("2001:db8::1/64".to_string());
        let mut empty_ipv6_gateway = ipv6_address();
        empty_ipv6_gateway.gateway = Some(String::new());

        let cases = [
            Case {
                scenario: "IPv4 gateway with IPv6 value",
                address: ipv4_gateway,
                expected_messages: &["`gateway`", "`2001:db8::1/64`", "declared V4 family"],
            },
            Case {
                scenario: "IPv6 host IP with IPv4 value",
                address: ipv6_ip,
                expected_messages: &["`ip`", "`192.0.2.10`", "declared V6 family"],
            },
            Case {
                scenario: "IPv4 interface prefix with IPv6 value",
                address: ipv4_interface_prefix,
                expected_messages: &[
                    "`interface_prefix`",
                    "`2001:db8::10/128`",
                    "declared V4 family",
                ],
            },
            Case {
                scenario: "IPv4 network prefix with IPv6 value",
                address: ipv4_prefix,
                expected_messages: &["`prefix`", "`2001:db8::/64`", "declared V4 family"],
            },
            Case {
                scenario: "IPv4 SVI with IPv6 value",
                address: ipv4_svi,
                expected_messages: &["`svi_ip`", "`2001:db8::2/64`", "declared V4 family"],
            },
            Case {
                scenario: "IPv4 loopback with IPv6 value",
                address: ipv4_loopback,
                expected_messages: &[
                    "`tenant_vrf_loopback_ip`",
                    "`2001:db8::3`",
                    "declared V4 family",
                ],
            },
            Case {
                scenario: "malformed host IP",
                address: malformed_ip,
                expected_messages: &["invalid `ip` value `not-an-ip`"],
            },
            Case {
                scenario: "malformed network prefix",
                address: malformed_prefix,
                expected_messages: &["invalid `prefix` value `not-a-prefix`"],
            },
            Case {
                scenario: "empty IPv4 gateway",
                address: empty_ipv4_gateway,
                expected_messages: &["invalid `gateway` value ``"],
            },
            Case {
                scenario: "empty SVI address",
                address: empty_svi,
                expected_messages: &["invalid `svi_ip` value ``"],
            },
            Case {
                scenario: "empty tenant VRF loopback",
                address: empty_loopback,
                expected_messages: &["invalid `tenant_vrf_loopback_ip` value ``"],
            },
            Case {
                scenario: "IPv6 gateway",
                address: ipv6_gateway,
                expected_messages: &["declared as V6", "`gateway`", "`2001:db8::1/64`"],
            },
            Case {
                scenario: "empty IPv6 gateway",
                address: empty_ipv6_gateway,
                expected_messages: &["declared as V6", "unsupported `gateway` value ``"],
            },
        ];

        for Case {
            scenario,
            address,
            expected_messages,
        } in cases
        {
            let mut interface = legacy_interface(vec![address]);
            let error = normalize_interface_addresses(&mut interface)
                .expect_err("invalid address value must be rejected");
            let message = format!("{error:#}");

            for expected in expected_messages {
                assert!(
                    message.contains(expected),
                    "{scenario}: expected `{expected}` in `{message}`"
                );
            }
        }
    }

    #[test]
    #[allow(deprecated)]
    fn network_config_normalizes_admin_and_every_tenant_interface() {
        let mut response = rpc::ManagedHostNetworkConfigResponse {
            admin_interface: Some(legacy_interface(vec![ipv4_address()])),
            tenant_interfaces: vec![
                legacy_interface(vec![ipv4_address()]),
                legacy_interface(vec![ipv6_address(), ipv4_address()]),
                legacy_interface(vec![ipv6_address()]),
            ],
            ..Default::default()
        };

        normalize_network_config_addresses(&mut response).unwrap();

        assert_eq!(response.admin_interface.unwrap(), expected_ipv4_interface());
        assert_eq!(response.tenant_interfaces[0], expected_ipv4_interface());
        assert_eq!(
            response.tenant_interfaces[1].addresses,
            vec![ipv6_address(), ipv4_address()]
        );
        assert_eq!(
            response.tenant_interfaces[1].ipv6_interface_config,
            Some(rpc::FlatInterfaceIpv6Config {
                ip: "2001:db8::1".to_string(),
                interface_prefix: "2001:db8::1/128".to_string(),
                svi_ip: Some("2001:db8::2/64".to_string()),
            })
        );
        assert_eq!(response.tenant_interfaces[2], expected_ipv6_interface());
    }

    #[test]
    fn public_address_selection_is_family_specific_and_stable() {
        let ipv4_low: Ipv4Addr = "192.0.2.10".parse().unwrap();
        let ipv6_low: Ipv6Addr = "2001:db8::10".parse().unwrap();

        scenarios!(
            run = |interfaces: Vec<(Option<u32>, Vec<&str>)>| {
                let interfaces = interfaces
                    .into_iter()
                    .map(|(virtual_function_id, addresses)| rpc::InstanceInterfaceStatus {
                        virtual_function_id,
                        addresses: addresses.into_iter().map(str::to_owned).collect(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>();
                select_public_addresses(&interfaces).map_err(drop)
            };
            "no addresses" {
                vec![] => Yields(PublicAddresses::default()),
            }

            "one address family" {
                vec![(None, vec!["192.0.2.20", "192.0.2.10"])] => Yields(PublicAddresses {
                    ipv4: Some(ipv4_low),
                    ipv6: None,
                }),
                vec![(None, vec!["2001:db8::20", "2001:db8::10"])] => Yields(PublicAddresses {
                    ipv4: None,
                    ipv6: Some(ipv6_low),
                }),
            }

            "dual-stack address order" {
                vec![(None, vec!["2001:db8::10", "192.0.2.20", "2001:db8::20", "192.0.2.10"])] => Yields(PublicAddresses {
                    ipv4: Some(ipv4_low),
                    ipv6: Some(ipv6_low),
                }),
                vec![(None, vec!["192.0.2.10", "2001:db8::20", "192.0.2.20", "2001:db8::10"])] => Yields(PublicAddresses {
                    ipv4: Some(ipv4_low),
                    ipv6: Some(ipv6_low),
                }),
            }

            "dual-stack physical interface order" {
                vec![
                    (None, vec!["192.0.2.20", "192.0.2.10"]),
                    (None, vec!["2001:db8::20", "2001:db8::10"]),
                ] => Yields(PublicAddresses {
                    ipv4: Some(ipv4_low),
                    ipv6: Some(ipv6_low),
                }),
                vec![
                    (None, vec!["2001:db8::10"]),
                    (Some(0), vec!["192.0.2.1"]),
                    (None, vec!["192.0.2.10"]),
                ] => Yields(PublicAddresses {
                    ipv4: Some(ipv4_low),
                    ipv6: Some(ipv6_low),
                }),
            }

            "invalid address" {
                vec![(None, vec!["not-an-address"])] => Fails,
            }
        );
    }
}
