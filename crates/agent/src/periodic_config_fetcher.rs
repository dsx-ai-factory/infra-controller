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
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ::rpc::forge_tls_client::ForgeClientConfig;
use ::rpc::{Instance, forge as rpc};
use arc_swap::ArcSwapOption;
use carbide_network::ip::{IdentifyAddressFamily, IpAddressFamily};
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineInterfaceId};
use config_version::ConfigVersion;
use eyre::Context;
use forge_dpu_agent_utils::utils::create_forge_client;
use ipnetwork::IpNetwork;
use tracing::{trace, warn};

use crate::instrumentation::ConfigFetch;
use crate::util::{get_periodic_dpu_config, get_sitename};

struct PeriodicFetcherState {
    config: PeriodicConfigFetcherConfig,
    netconf: ArcSwapOption<rpc::ManagedHostNetworkConfigResponse>,
    instmeta: ArcSwapOption<InstanceMetadata>,
    is_cancelled: AtomicBool,
    sitename: Option<String>,
}

/// Fetches the desired network configuration for a managed host in regular intervals
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
    pub(super) fn net_conf_read(&self) -> Option<Arc<rpc::ManagedHostNetworkConfigResponse>> {
        self.state.netconf.load_full()
    }

    pub(super) fn meta_data_conf_reader(&self) -> Option<Arc<InstanceMetadata>> {
        self.state.instmeta.load_full()
    }
}

impl Drop for PeriodicConfigFetcher {
    fn drop(&mut self) {
        // Signal the background task and wait for it to shut down
        // TODO: Might be nicer if it would be interrupted during waiting for 30s
        self.state
            .is_cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(jh) = self.join_handle.take() {
            tokio::spawn(async move {
                jh.await.unwrap();
            });
        }
    }
}

impl PeriodicConfigFetcher {
    pub(super) async fn new(config: PeriodicConfigFetcherConfig) -> Self {
        let forge_client_config = Arc::clone(&config.forge_client_config);
        // Fetch the sitename from Carbide at the start and keep it in State
        // so that it can be made available as instance metadata.
        let sitename = match fetch_sitename(&forge_client_config, &config.forge_api).await {
            Ok(sn) => sn,
            Err(e) => {
                warn!(error = %e, "Unable to fetch sitename");
                None
            }
        };

        let state = Arc::new(PeriodicFetcherState {
            netconf: ArcSwapOption::default(),
            instmeta: ArcSwapOption::default(),
            sitename,
            config,
            is_cancelled: AtomicBool::new(false),
        });

        // Do an initial synchronous fetch so that caller has data to use
        // This gets a DPU on the network immediately
        single_fetch(&forge_client_config, state.clone()).await;

        let task_state = state.clone();
        let join_handle = tokio::spawn(async move {
            while single_fetch(&forge_client_config, task_state.clone()).await {
                tokio::time::sleep(task_state.config.config_fetch_interval).await;
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
            .as_ref()
            .and_then(|netconf| netconf.host_interface_id.as_ref())
            .and_then(|id| id.parse().ok())
    }
}

pub(super) struct PeriodicConfigFetcherConfig {
    /// The interval in which the config is fetched
    pub(super) config_fetch_interval: Duration,
    pub(super) machine_id: DpuMachineId,
    pub(super) forge_api: String,
    pub(super) forge_client_config: Arc<ForgeClientConfig>,
}

// Use the version grpc call to carbide to get
// the sitename. This will be made visible to tenant OS
// at an FMDS endpoint
async fn fetch_sitename(
    forge_client_config: &ForgeClientConfig,
    forge_api: &str,
) -> Result<Option<String>, eyre::Report> {
    let mut client = create_forge_client(forge_api, forge_client_config).await?;

    get_sitename(&mut client).await
}

async fn single_fetch(
    forge_client_config: &ForgeClientConfig,
    state: Arc<PeriodicFetcherState>,
) -> bool {
    if state
        .is_cancelled
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        trace!("Periodic fetcher was dropped. Stopping config reading");
        return false;
    }

    trace!(
        machine_id = %state.config.machine_id,
        "Fetching periodic configuration"
    );

    let result = fetch(
        &state.config.machine_id,
        &state.config.forge_api,
        forge_client_config,
    )
    .await;
    // The outcome covers the whole fetch: a successful RPC whose instance
    // metadata fails to convert is still a failed configuration fetch.
    match result {
        Ok(resp) => {
            state.netconf.store(Some(Arc::new(resp.clone())));

            match instance_metadata_from_instance(resp.instance, state.sitename.clone()) {
                Ok(Some(config)) => {
                    state.instmeta.store(Some(Arc::new(config)));
                    ConfigFetch::Succeeded.emit();
                }
                Ok(None) => {
                    state.instmeta.store(None);
                    ConfigFetch::Succeeded.emit();
                }
                Err(err) => ConfigFetch::Failed {
                    error: err.to_string(),
                    retry_interval_seconds: state.config.config_fetch_interval.as_secs_f64(),
                }
                .emit(),
            }
        }
        Err(err) => match err.downcast_ref::<tonic::Status>() {
            Some(grpc_status) if grpc_status.code() == tonic::Code::NotFound => {
                state.netconf.store(None);
                state.instmeta.store(None);
                ConfigFetch::NotFound {
                    machine_id: state.config.machine_id.to_string(),
                }
                .emit();
            }
            _ => ConfigFetch::Failed {
                error: format!("{err:?}"),
                retry_interval_seconds: state.config.config_fetch_interval.as_secs_f64(),
            }
            .emit(),
        },
    }

    true
}

/// Make the network request to get network config
async fn fetch(
    dpu_machine_id: &MachineId,
    forge_api: &str,
    client_config: &ForgeClientConfig,
) -> Result<rpc::ManagedHostNetworkConfigResponse, eyre::Report> {
    let mut client = create_forge_client(forge_api, client_config).await?;

    let mut response = get_periodic_dpu_config(&mut client, dpu_machine_id).await?;
    normalize_network_config_addresses(&mut response)?;
    Ok(response)
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
    use carbide_test_support::scenarios;

    use super::*;

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
            interface_prefix: "2001:db8::/127".to_string(),
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
                interface_prefix: "2001:db8::/127".to_string(),
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
            interface_prefix: "2001:db8::/127".to_string(),
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
            interface_prefix: "2001:db8::/127".to_string(),
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
                interface_prefix: "2001:db8::/127".to_string(),
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
