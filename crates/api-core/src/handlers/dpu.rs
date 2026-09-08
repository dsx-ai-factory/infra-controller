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

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv6Addr};
use std::str::FromStr;
use std::time::Duration;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::model::{RpcInto, RpcTryFrom};
use ::rpc::{common as rpc_common, forge as rpc};
use carbide_dpf::{DpuDeploymentType, dpu_cr_name, dpu_node_cr_name};
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_secrets::credentials::{BgpCredentialType, CredentialKey, Credentials};
use carbide_utils::arch::CpuArchitecture;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{DpuMachineId, MachineId};
use db::vpc_prefix::VpcId;
use db::{
    DatabaseError, ObjectColumnFilter, dpu_agent_upgrade_policy, network_security_group,
    network_segment,
};
use futures_util::future::join_all;
use ipnetwork::IpNetwork;
use itertools::Itertools;
use model::extension_service::{
    ExtensionService, ExtensionServiceType, ExtensionServiceVersionInfo,
};
use model::hardware_info::{MachineInventory, MachineInventorySoftwareComponent};
use model::instance::config::extension_services::InstanceExtensionServiceConfig;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::network::MachineNetworkStatusObservation;
use model::machine::upgrade_policy::{AgentUpgradePolicy, BuildVersion};
use model::machine::{
    InstanceState, LoadSnapshotOptions, ManagedHostState, ManagedHostStateSnapshot,
};
use model::machine_update_module::HOST_UPDATE_HEALTH_PROBE_ID;
use model::network_segment::NetworkSegmentSearchConfig;
use model::rack_type::select_dpu_nvconfig_profile;
use tonic::{Request, Response, Status};

use crate::api::{Api, log_machine_id, log_request_data};
use crate::cfg::file::VpcIsolationBehaviorType;
use crate::handlers::astra::{get_astra_config, process_astra_config_status};
use crate::handlers::extension_service;
use crate::handlers::utils::{StateHandlerWakeupFailed, WakeupTrigger, convert_and_log_machine_id};
use crate::{CarbideError, cfg, ethernet_virtualization};

/// vxlan48 is special HBN single vxlan device. It handles networking between machines on the
/// same subnet. It handles the encapsulation into VXLAN and VNI for cross-host comms.
const HBN_SINGLE_VLAN_DEVICE: &str = "vxlan48";

/// Bounds Kubernetes label reads while Site Explorer attachment locks are held.
const DPF_DEPLOYMENT_LABEL_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Consolidates host-level and DPU-level `ManagedHostNetworkConfig` into
/// the single proto sent to `carbide-dpu-agent`. The host layer
/// contributes shared fields (e.g. `use_admin_network`); the DPU layer
/// contributes per-DPU fields (e.g. `loopback_ip`). The IPv6 loopback is
/// present only for FNN so other agents keep their IPv4-only wire contract.
fn build_consolidated_network_config(
    host_network_config: &model::machine::network::ManagedHostNetworkConfig,
    dpu_loopback_ip: IpAddr,
    dpu_loopback_ip_v6: Option<Ipv6Addr>,
    network_virtualization_type: VpcVirtualizationType,
) -> rpc::ManagedHostNetworkConfig {
    rpc::ManagedHostNetworkConfig {
        loopback_ip: dpu_loopback_ip.to_string(),
        quarantine_state: host_network_config.quarantine_state.clone().map(Into::into),
        loopback_ip_v6: dpu_loopback_ip_v6
            .filter(|_| network_virtualization_type == VpcVirtualizationType::Fnn)
            .map(|ip| ip.to_string()),
    }
}

/// FNN renders family-specific deny policies, while ETV exposes only its IPv4 policy. Filter at
/// this per-DPU boundary so a mixed site can send IPv6 to FNN without changing the IPv4-only wire
/// contract for ETV, Flat, and older agents.
fn deny_prefixes_for_agent(
    prefixes: &[IpNetwork],
    network_virtualization_type: VpcVirtualizationType,
) -> Vec<String> {
    let supports_ipv6 = network_virtualization_type == VpcVirtualizationType::Fnn;

    prefixes
        .iter()
        .filter(|prefix| supports_ipv6 || prefix.is_ipv4())
        .map(ToString::to_string)
        .collect()
}

/// Builds the deprecated deny field with the same address-family contract as `deny_prefixes`.
///
/// Mutual isolation folds site-fabric prefixes into this field, so those prefixes must pass
/// through the per-DPU filter as well.
fn deprecated_deny_prefixes_for_agent(
    deny_prefixes: &[String],
    site_fabric_prefixes: &[IpNetwork],
    isolation_behavior: VpcIsolationBehaviorType,
    network_virtualization_type: VpcVirtualizationType,
) -> Vec<String> {
    match isolation_behavior {
        VpcIsolationBehaviorType::MutualIsolation => {
            let site_fabric_prefixes =
                deny_prefixes_for_agent(site_fabric_prefixes, network_virtualization_type);
            [site_fabric_prefixes.as_slice(), deny_prefixes].concat()
        }
        VpcIsolationBehaviorType::Open => deny_prefixes.to_vec(),
    }
}

/// `preferred_physical_ip` chooses a stable address for the fallback tenant
/// hostname. IPv4 remains the familiar name for a dual-stack interface, while
/// choosing the lowest address within each family keeps the result independent
/// of `HashMap` iteration order.
fn preferred_physical_ip(ip_addresses: impl IntoIterator<Item = IpAddr>) -> Option<IpAddr> {
    ip_addresses
        .into_iter()
        .min_by_key(|ip_address| (!ip_address.is_ipv4(), *ip_address))
}

/// `tenant_interface_fqdn` keeps the tenant's hostname intact when one was
/// provided. Otherwise, the physical address becomes a valid DNS label through
/// the same formatter used by IP-based host naming. The caller passes no
/// physical address only when the interface has a prefix but no address;
/// `tenant_interface_fqdn` then uses the stable instance UUID.
fn tenant_interface_fqdn(
    tenant_hostname: Option<&str>,
    physical_ip: Option<&IpAddr>,
    instance_id: &InstanceId,
    domain: &str,
) -> Result<String, DatabaseError> {
    let hostname = if let Some(hostname) = tenant_hostname {
        hostname.to_string()
    } else if let Some(physical_ip) = physical_ip {
        db::host_naming::address_to_hostname(physical_ip)?
    } else {
        instance_id.to_string()
    };

    Ok(format!("{hostname}.{domain}"))
}

async fn get_managed_host_network_config_inner(
    api: &Api,
    dpu_machine_id: DpuMachineId,
) -> Result<rpc::ManagedHostNetworkConfigResponse, tonic::Status> {
    let mut txn = api.txn_begin().await?;

    let snapshot = db::managed_host::load_snapshot(
        &mut txn,
        &dpu_machine_id,
        LoadSnapshotOptions::default().with_host_health(api.runtime_config.host_health),
    )
    .await?
    .ok_or(CarbideError::NotFoundError {
        kind: "machine",
        id: dpu_machine_id.to_string(),
    })?;

    let dpu_snapshot = match snapshot
        .dpu_snapshots
        .iter()
        .find(|s| s.id == dpu_machine_id.into())
    {
        Some(dpu_snapshot) => dpu_snapshot,
        None => {
            return Err(CarbideError::FailedPrecondition(format!(
                "DPU {dpu_machine_id} needs discovery.  DPU snapshot not found for managed host"
            ))
            .into());
        }
    };

    let mut maybe_instance =
        Option::<rpc::Instance>::rpc_try_from(snapshot.clone()).map_err(CarbideError::from)?;

    let primary_dpu_snapshot = snapshot
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|x| x.primary_interface)
        .ok_or_else(|| CarbideError::internal("primary interface is missing".to_string()))?;

    let primary_dpu = db::machine_interface::find_one(&mut txn, primary_dpu_snapshot.id).await?;
    let is_primary_dpu = primary_dpu
        .attached_dpu_machine_id
        .map(|x| dpu_snapshot.id == x.into())
        .unwrap_or(false);

    let loopback_ip = match dpu_snapshot.loopback_ip() {
        Some(ip) => ip,
        None => {
            return Err(CarbideError::FailedPrecondition(format!(
                "DPU {dpu_machine_id} needs discovery. does not have a loopback IP yet"
            ))
            .into());
        }
    };

    // its ok if there is no locator here.  if there isn't one, then only the primary dpu is allowed to be configred (checked below)
    let device_locator = snapshot
        .host_snapshot
        .get_device_locator_for_dpu_id(&dpu_machine_id)
        .ok();

    let dpu_has_tenant_interface_config =
        snapshot
            .instance
            .as_ref()
            .is_some_and(|interface_snapshot| {
                interface_snapshot
                    .config
                    .network
                    .interfaces
                    .iter()
                    .any(|interface_config| {
                        (interface_config.device_locator.is_none() && is_primary_dpu)
                            || (interface_config.device_locator.is_some()
                                && device_locator == interface_config.device_locator)
                    })
            });

    // If there is an instance, the state machine sets the host to tenant
    // network. But if no interfaces are configured for this DPU, override
    // and keep it on admin. This prevents the host from using the DPU at all.
    let use_admin_network = snapshot.use_admin_network() || !dpu_has_tenant_interface_config;

    let use_admin_network_changed = dpu_snapshot.network_config.use_admin_network_changed;

    let mut network_virtualization_type = VpcVirtualizationType::EthernetVirtualizer;

    let mut use_fnn_over_admin_nw = false;

    // If FNN config is enabled, we should use it in admin network.
    if let Some(fnn) = &api.runtime_config.fnn
        && let Some(admin) = &fnn.admin_vpc
        && admin.enabled
    {
        use_fnn_over_admin_nw = true;
        network_virtualization_type = VpcVirtualizationType::Fnn;
    }
    let use_vpc_vrf_loopback = api
        .runtime_config
        .fnn
        .as_ref()
        .is_some_and(|c| c.use_vpc_vrf_loopback);

    let booturl_override = if snapshot
        .host_snapshot
        .status
        .hardware_info
        .as_ref()
        .map(|h| h.machine_type)
        == Some(CpuArchitecture::X86_64)
    {
        api.runtime_config.x86_pxe_boot_url_override.clone()
    } else {
        api.runtime_config.arm_pxe_boot_url_override.clone()
    };

    let admin_vpc_routing_profile = api
        .runtime_config
        .fnn
        .as_ref()
        .and_then(|f| f.admin_vpc.as_ref())
        .map(|v| &v.routing_profile);

    let (admin_interface_rpc, host_interface_id) = ethernet_virtualization::admin_network(
        &mut txn,
        &snapshot,
        &dpu_machine_id,
        ethernet_virtualization::AdminNetworkOptions {
            fnn_enabled: use_fnn_over_admin_nw,
            common_pools: &api.common_pools,
            booturl: &booturl_override,
            use_vpc_vrf_loopback,
            routing_profile: admin_vpc_routing_profile,
        },
    )
    .await?;

    // If admin network is in use and is fnn, use admin network's vpc_vni.
    let mut vpc_vni = if use_admin_network && admin_interface_rpc.vpc_vni != 0 {
        Some(admin_interface_rpc.vpc_vni)
    } else {
        None
    };

    let tenant_interfaces = match &snapshot.instance {
        None => vec![],
        // We don't support secondary DPU yet.
        // If admin network is to be used for this managedhost, why to send old tenant data, which
        // is just to be deleted.
        Some(_instance) if use_admin_network => vec![],
        Some(_instance)
            // If instance is waiting for network segment to come up in READY state, stay on admin
            // network.
            if matches!(
                snapshot.managed_state,
                ManagedHostState::Assigned {
                    instance_state: InstanceState::WaitingForNetworkSegmentToBeReady,
                }
            ) =>
        {
            // Should/Can we still query and return the NSG of the VPC so that
            // policies can be configured on the DPU while interfaces are still coming up?
            vec![]
        }
        Some(instance) => {
            let interfaces = &instance.config.network.interfaces;
            let Some(network_segment_id) = interfaces[0].network_segment_id else {
                // Network segment allocation is done before persisting record in db. So if still
                // network segment is empty, return error.
                return Err(CarbideError::NetworkSegmentNotAllocated.into());
            };
            let Some(vpc) = db::vpc::find_by_segment(&mut txn, network_segment_id)
                .await? else {
                return Err(CarbideError::FailedPrecondition(
                    "network segment is not a member of a VPC".to_string(),
                ).into())
            };

            network_virtualization_type = vpc.config.network_virtualization_type;

            vpc_vni = vpc.status.vni.map(|x| x as u32);

            let suppress_tenant_security_groups = match &snapshot.managed_state {
                ManagedHostState::Assigned { instance_state } => {
                    // Within the BootingWithDiscoveryImage state, we use the
                    // tenant's network to boot the discovery/scout image via
                    // PXE, and then phone home via HTTPS to the API to signal
                    // that the machine is no longer running the tenant OS (at
                    // which point it's safe to move to the admin network). The
                    // tenant's NSGs can interfere with these connections, so we
                    // must avoid installing them.
                    matches!(instance_state, InstanceState::BootingWithDiscoveryImage { ..})
                },
                _ => false,
            };

            // Check if there's an NSG on the instance.
            let network_security_group_details = if !suppress_tenant_security_groups
                && let Some((tenant_id, Some(nsg_id))) = snapshot.instance.as_ref().map(|i| {
                (
                    &i.config.tenant.tenant_organization_id,
                    i.config.network_security_group_id.as_ref(),
                )
            }) {
                // Make our DB query for the IDs to get our NetworkSecurityGroup
                let network_security_group =
                    network_security_group::find_by_ids(
                        &mut txn,
                        std::slice::from_ref(nsg_id),
                        Some(tenant_id),
                        false,
                    )
                        .await?
                        .pop()
                        .ok_or(CarbideError::NotFoundError {
                            kind: "NetworkSecurityGroup",
                            id: tenant_id.to_string(),
                        })?;

                Some((
                    i32::from(rpc::NetworkSecurityGroupSource::NsgSourceInstance),
                    network_security_group,
                ))
            } else {
                None
            };

            let mut tenant_interfaces = Vec::with_capacity(interfaces.len());

            let physical_iface = interfaces.iter().find(|x| {
                rpc::InterfaceFunctionType::from(x.function_id.function_type())
                    == rpc::InterfaceFunctionType::Physical
            });

            let Some(physical_iface) = physical_iface else {
                return Err(CarbideError::internal(String::from(
                    "Physical interface not found",
                ))
                .into());
            };

            let physical_ip = preferred_physical_ip(physical_iface.ip_addrs.values().copied());
            let prefix_only =
                physical_iface.ip_addrs.is_empty() && !physical_iface.interface_prefixes.is_empty();
            if physical_ip.is_none() && !prefix_only {
                return Err(CarbideError::internal(String::from(
                    "physical IP address not found",
                ))
                .into());
            }

            // All interfaces have the segment id allocated. It is already validated during
            // instance creation.
            let segment_ids = interfaces.iter().filter_map(|x|x.network_segment_id).collect_vec();
            let segment_details = db::network_segment::find_by(
                &mut txn,
                ObjectColumnFilter::List(network_segment::IdColumn, &segment_ids),
                NetworkSegmentSearchConfig::default(),
            ).await?;

            let segment_details = segment_details.iter().map(|x|(x.id, x)).collect::<HashMap<_,_>>();
            let mut tenant_loopback_ips: HashMap<VpcId, IpAddr> = HashMap::new();

            // Resolve every segment domain in a single query up front, then look each one up by id
            // inside the interface loop. The domains map keeps its rows keyed by id so a missing
            // entry still surfaces the same NotFoundError the per-interface lookup used to raise.
            let subdomain_ids = segment_details
                .values()
                .filter_map(|segment| segment.config.subdomain_id)
                .unique()
                .collect_vec();
            let domains_by_id =
                db::dns::domain::find_by_uuids(txn.as_pgconn(), &subdomain_ids)
                    .await
                    .map_err(CarbideError::from)?;

            // if there is no device then this is a legacy config and only the primary dpu is allowed.
            // all other DPUs don't get interfaces
            for iface in interfaces.iter().filter(|i|
                (i.device_locator.is_none() && is_primary_dpu) || (i.device_locator.as_ref().is_some_and(|dl| device_locator.as_ref().is_some_and(|dl2| dl2 == dl)))
            ) {
                // This can not happen as validated during instance creation.
                let Some(iface_segment) = iface.network_segment_id else {
                    return Err(CarbideError::Internal { message: format!(
                        "Tenant segment is not assigned for iface: {iface:?}."
                    ) }.into());
                };

                let Some(segment) = segment_details.get(&iface_segment) else {
                    return Err(CarbideError::Internal { message: format!(
                        "Tenant segment id {iface_segment} is not found in db. Can not fetch the details."
                    ) }.into());
                };

                let tenant_loopback_ip = if VpcVirtualizationType::Fnn == network_virtualization_type
                    && use_vpc_vrf_loopback
                {
                    match segment.config.vpc_id {
                        Some(vpc_id) => {
                            if let Some(loopback_ip) = tenant_loopback_ips.get(&vpc_id) {
                                Some(*loopback_ip)
                            } else {
                                // Resolve loopbacks after the interface segment is known so each VPC
                                // receives its own DPU loopback allocation.
                                let loopback_ip =
                                    db::vpc_dpu_loopback::get_or_allocate_loopback_ip_for_vpc(
                                        &api.common_pools,
                                        &mut txn,
                                        &dpu_machine_id,
                                        &vpc_id,
                                    )
                                    .await?;

                                tenant_loopback_ips.insert(vpc_id, loopback_ip);
                                Some(loopback_ip)
                            }
                        }
                        None => None,
                    }
                } else {
                    None
                };

                // Build the FQDN from this interface's segment domain.
                let domain = match segment.config.subdomain_id {
                    Some(domain_id) => domains_by_id
                        .get(&domain_id)
                        .ok_or_else(|| CarbideError::NotFoundError {
                            kind: "domain",
                            id: domain_id.to_string(),
                        })?
                        .name
                        .clone(),
                    None => "unknowndomain".to_string(),
                };
                let fqdn = tenant_interface_fqdn(
                    instance.config.tenant.hostname.as_deref(),
                    physical_ip.as_ref(),
                    &instance.id,
                    &domain,
                )?;

                let tenant_interface = ethernet_virtualization::tenant_network(
                    &mut txn,
                    instance.id,
                    iface,
                    fqdn,
                    tenant_loopback_ip,
                    network_virtualization_type,
                    suppress_tenant_security_groups,
                    network_security_group_details.clone(),
                    segment,
                    match api.runtime_config.vpc_peering_policy_on_existing {
                        None => api.runtime_config.vpc_peering_policy,
                        Some(vpc_peering_policy) => Some(vpc_peering_policy),
                    },
                    &booturl_override,
                    api.runtime_config.fnn.as_ref(),
                )
                .await?;

                tenant_interfaces.push(tenant_interface);
            }

            tenant_interfaces
        }
    };

    // Deprecated compatibility field for DPU agents that do not yet read
    // FlatInterfaceConfig.vpc_routing_profile.
    let deprecated_routing_profile = if tenant_interfaces.is_empty() {
        admin_vpc_routing_profile.map(rpc::RoutingProfile::from)
    } else {
        tenant_interfaces
            .first()
            .and_then(|interface| interface.vpc_routing_profile.clone())
    };

    let network_config = build_consolidated_network_config(
        &snapshot.host_snapshot.network_config.value,
        loopback_ip,
        dpu_snapshot.loopback_ip_v6(),
        network_virtualization_type,
    );

    let asn = if network_virtualization_type == VpcVirtualizationType::Fnn {
        dpu_snapshot.asn.ok_or_else(|| {
            let message = format!(
                "FNN configured but DPU {} has not been assigned an ASN",
                dpu_snapshot.id
            );

            tracing::error!(
                dpu_machine_id = %dpu_snapshot.id,
                "FNN-configured DPU has no ASN"
            );
            CarbideError::internal(message)
        })?
    } else {
        api.eth_data.asn
    };

    let deny_prefixes =
        deny_prefixes_for_agent(&api.eth_data.deny_prefixes, network_virtualization_type);

    let site_fabric_networks = api
        .eth_data
        .site_fabric_prefixes
        .as_ref()
        .map(|s| s.as_ip_slice())
        .unwrap_or_default();
    let site_fabric_prefixes: Vec<String> = site_fabric_networks
        .iter()
        .map(|net| net.to_string())
        .collect();

    let deprecated_deny_prefixes = deprecated_deny_prefixes_for_agent(
        &deny_prefixes,
        site_fabric_networks,
        api.runtime_config.vpc_isolation_behavior,
        network_virtualization_type,
    );

    // Strip the source_type for the route servers that we feed back to the DPUs -- they just care
    // about the IP address. Although, maybe in the future, we might be interested in sending the
    // entire struct down, and then putting some smarts inside the DPU re: the source_type.
    // Only pass them on if route servers are enabled.
    let route_servers = if api.runtime_config.enable_route_servers {
        db::route_servers::get(&mut txn)
            .await?
            .into_iter()
            .map(|rs| rs.address.to_string())
            .collect()
    } else {
        vec![]
    };

    // If instance is present, get the extension services configured for the instance.

    // simple grouping of stuff we need from the extension service:
    struct ExtensionServiceInfo<'a> {
        service: ExtensionService,
        version: ExtensionServiceVersionInfo,
        instance_config: &'a InstanceExtensionServiceConfig,
    }

    // First fetch from the database, while we have a transaction:
    let extension_service_info = if let Some(instance) = snapshot.instance.as_ref() {
        let service_configs = &instance.config.extension_services.service_configs;

        // Fetch every configured extension service in one query, then index by id so each config
        // resolves its service from memory instead of issuing a query per config.
        let service_ids = service_configs
            .iter()
            .map(|config| config.service_id)
            .unique()
            .collect_vec();
        let services_by_id =
            db::extension_service::find_by_ids(&mut txn, &service_ids, false, false)
                .await?
                .into_iter()
                .map(|service| (service.id, service))
                .collect::<HashMap<_, _>>();

        // The nested Instance is also consumed by the agent. Keep its
        // extension-service view aligned with the dedicated agent payload so
        // a DPF service cannot leak through that compatibility field either.
        let agent_service_ids: HashSet<_> = services_by_id
            .values()
            .filter(|service| {
                service.service_type == ExtensionServiceType::KubernetesPod
                    && service.deleted.is_none()
            })
            .map(|service| service.id.to_string())
            .collect();
        if let Some(instance) = maybe_instance.as_mut()
            && let Some(config) = instance.config.as_mut()
            && let Some(extension_services) = config.dpu_extension_services.as_mut()
        {
            extension_services
                .service_configs
                .retain(|config| agent_service_ids.contains(&config.service_id));
            if extension_services.service_configs.is_empty() {
                config.dpu_extension_services = None;
            }
        }

        let mut extension_service_info: Vec<ExtensionServiceInfo> =
            Vec::with_capacity(service_configs.len());
        for config in service_configs {
            let service = services_by_id
                .get(&config.service_id)
                .cloned()
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "ExtensionService",
                    id: config.service_id.to_string(),
                })?;

            // A DPF Helm service may be active or retained as a removed
            // attachment while DPF finishes its own cleanup. In either case
            // it must not produce agent configuration, trigger a version
            // lookup, or read a credential from Vault.
            if service.service_type == ExtensionServiceType::DpfHelmChart {
                continue;
            }

            // The pinned version is looked up individually so the exact
            // `version == config.version` selection (and its full data/credential/observability
            // payload) is preserved per config. The batched `services_by_id` lookup above already
            // established the service exists, so this skips the per-service existence probe.
            let version = db::extension_service::find_version_info_of_known_service(
                &mut txn,
                config.service_id,
                Some(config.version),
            )
            .await?;

            extension_service_info.push(ExtensionServiceInfo {
                service,
                version,
                instance_config: config,
            });
        }
        extension_service_info
    } else {
        Vec::new()
    };

    // Next, get credentials for each extension service from vault. This should be done after the
    // transaction is committed.
    txn.commit().await?;
    let extension_services = join_all(extension_service_info.into_iter().map(|info| async move {
        // Get the credential if it exists
        let credential = if info.version.has_credential {
            let key = extension_service::create_extension_service_credential_key(
                &info.service.id,
                info.version.version,
            );
            Some(
                extension_service::get_extension_service_credential(&api.credential_manager, key)
                    .await?,
            )
        } else {
            None
        };

        Ok::<_, tonic::Status>(rpc::ManagedHostDpuExtensionServiceConfig {
            service_id: info.service.id.to_string(),
            name: info.service.name,
            removed: info.instance_config.removed.map(|ts| ts.to_string()),
            version: info.version.version.to_string(),
            service_type: rpc::DpuExtensionServiceType::from(info.service.service_type.clone())
                .into(),
            data: info.version.data,
            credential,
            observability: info.version.observability.map(|o| o.into()),
        })
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;

    let astra_config = get_astra_config(api, &snapshot).await?;

    let resp = rpc::ManagedHostNetworkConfigResponse {
        instance_id: snapshot.instance.as_ref().map(|instance| instance.id),
        asn,
        dhcp_servers: api
            .eth_data
            .dhcp_servers
            .iter()
            .map(|addr| addr.to_string())
            .collect(),
        route_servers,
        ntp_servers: api
            .runtime_config
            .ntp_servers
            .iter()
            .map(|addr| addr.to_string())
            .collect(),
        // TODO: Automatically add the prefix(es?) from the IPv4 loopback
        // pool to deny_prefixes. The database stores the pool in an
        // exploded representation, so we either need to reconstruct the
        // original prefix from what's in the database, or find some way to
        // store it when it's added or resized.
        deprecated_deny_prefixes,
        deny_prefixes,
        site_fabric_prefixes,
        anycast_site_prefixes: api
            .runtime_config
            .anycast_site_prefixes
            .iter()
            .map(|p| p.to_string())
            .collect(),
        tenant_host_asn: api.runtime_config.common_tenant_host_asn,
        datacenter_asn: api.runtime_config.datacenter_asn,
        vpc_isolation_behavior: rpc::VpcIsolationBehaviorType::from(
            api.runtime_config.vpc_isolation_behavior,
        )
        .into(),
        vni_device: if use_admin_network {
            "".to_string()
        } else {
            HBN_SINGLE_VLAN_DEVICE.to_string()
        },
        site_global_vpc_vni: api.runtime_config.site_global_vpc_vni,
        managed_host_config: Some(network_config),
        managed_host_config_version: snapshot
            .host_snapshot
            .network_config
            .version
            .version_string(),
        use_admin_network,
        admin_interface: Some(admin_interface_rpc),
        tenant_interfaces,
        network_security_policy_overrides: api
            .runtime_config
            .network_security_group
            .policy_overrides
            .iter()
            .map(|r| ethernet_virtualization::resolve_security_group_rule(r.clone()))
            .collect::<Result<Vec<rpc::ResolvedNetworkSecurityGroupRule>, CarbideError>>()?,
        stateful_acls_enabled: api
            .runtime_config
            .network_security_group
            .stateful_acls_enabled,
        instance_network_config_version: if use_admin_network {
            "".to_string()
        } else {
            snapshot
                .instance
                .unwrap()
                .network_config_version
                .version_string()
        },
        remote_id: dpu_machine_id.remote_id(),
        network_virtualization_type: Some(
            rpc::VpcVirtualizationType::from(network_virtualization_type).into(),
        ),
        vpc_vni,
        // Deprecated: this field is always true now.
        // This should be removed in future version.
        enable_dhcp: true,
        host_interface_id: Some(host_interface_id.to_string()),
        is_primary_dpu,
        min_dpu_functioning_links: api.runtime_config.min_dpu_functioning_links,
        dpu_network_pinger_type: api.runtime_config.dpu_network_monitor_pinger_type.clone(),
        internet_l3_vni: Some(api.runtime_config.internet_l3_vni), // Deprecated.  Remove when all agents and controllers are on a version that doesn't expect this.
        common_internal_route_target: api.runtime_config.fnn.as_ref().and_then(|c| {
            c.common_internal_route_target
                .as_ref()
                .map(|rt| rpc_common::RouteTarget {
                    asn: rt.asn,
                    vni: rt.vni,
                })
        }),
        routing_profile: deprecated_routing_profile,

        additional_route_target_imports: api
            .runtime_config
            .fnn
            .as_ref()
            .map(|c| {
                c.additional_route_target_imports
                    .iter()
                    .map(|i| rpc_common::RouteTarget {
                        asn: i.asn,
                        vni: i.vni,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        instance: maybe_instance,
        dpu_extension_services: extension_services,
        bgp_leaf_session_password: match api.runtime_config.bgp_leaf_session_password.as_ref() {
            Some(p) => match p {
                cfg::file::BgpLeafSessionPassword::SiteWide => Some(
                    get_bgp_password(
                        &api.credential_manager,
                        CredentialKey::Bgp {
                            credential_type: BgpCredentialType::SiteWideLeafPassword,
                        },
                    )
                    .await?,
                ),
            },
            None => None,
        },
        astra_config,
        use_admin_network_changed,
    };

    // If this all worked, we shouldn't emit a log line
    tracing::Span::current().record("logfmt.suppress", true);

    Ok(resp)
}

pub(crate) async fn get_managed_host_network_config(
    api: &Api,
    request: Request<rpc::ManagedHostNetworkConfigRequest>,
) -> Result<tonic::Response<rpc::ManagedHostNetworkConfigResponse>, tonic::Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let dpu_machine_id =
        convert_and_log_machine_id::<DpuMachineId>(request.dpu_machine_id.as_ref())?;

    let resp = get_managed_host_network_config_inner(api, dpu_machine_id).await?;

    Ok(Response::new(resp))
}

pub(crate) async fn update_agent_reported_inventory(
    api: &Api,
    request: Request<rpc::DpuAgentInventoryReport>,
) -> Result<Response<()>, tonic::Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let dpu_machine_id = convert_and_log_machine_id::<DpuMachineId>(request.machine_id.as_ref())?;

    // For DPF-ingested DPUs the agent runs containerized and cannot enumerate
    // the DPF services directly. Read service versions from the DPF operator
    // on every inventory report so the DB stays current after upgrades.
    let mut txn = api.txn_begin().await?;
    let host_snapshot =
        db::managed_host::load_snapshot(&mut txn, &dpu_machine_id, LoadSnapshotOptions::default())
            .await?;
    txn.commit().await?;

    if let Some(snapshot) = host_snapshot
        && snapshot.host_snapshot.config.dpf.used_for_ingestion
    {
        let machine = snapshot
            .dpu_snapshots
            .iter()
            .find(|d| d.id == dpu_machine_id.into())
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "dpu",
                id: dpu_machine_id.to_string(),
            })?;

        let dpf_sdk = api.dpf_sdk.as_ref().ok_or_else(|| {
            CarbideError::internal(format!(
                "dpf SDK unavailable but DPU {dpu_machine_id} was ingested via DPF"
            ))
        })?;

        // Both BMC MACs are needed to build the DPU CR name queried from the DPF
        // operator. If either is not yet recorded, skip the DPF inventory update
        // for this report rather than rejecting it; a later heartbeat retries once
        // the MACs are known.
        let (Some(dpu_device_id), Some(host_node_id)) =
            (machine.dpf_id(), snapshot.host_snapshot.dpf_id())
        else {
            tracing::debug!(
                machine_id = %dpu_machine_id,
                "skipping DPF service inventory update: DPU or host BMC MAC not yet known"
            );
            return Ok(Response::new(()));
        };
        let dpu_name = dpu_cr_name(&dpu_device_id, &host_node_id);

        let service_versions = dpf_sdk
            .get_service_versions_for_dpu(&dpu_name)
            .await
            .map_err(|e| CarbideError::internal(e.to_string()))?;

        let inventory = MachineInventory {
            components: service_versions
                .into_iter()
                .map(|v| MachineInventorySoftwareComponent {
                    name: v.name,
                    version: v.version,
                    url: v.url,
                })
                .collect(),
        };

        let mut txn = api.txn_begin().await?;
        db::machine::update_agent_reported_inventory(&mut txn, &dpu_machine_id, &inventory).await?;
        txn.commit().await?;

        tracing::debug!(
            machine_id = %dpu_machine_id,
            component_count = inventory.components.len(),
            "updated DPF service inventory from operator",
        );
        return Ok(Response::new(()));
    }

    if let Some(inventory) = request.inventory.as_ref() {
        let mut txn = api.txn_begin().await?;

        let inventory =
            MachineInventory::try_from(inventory.clone()).map_err(CarbideError::from)?;
        db::machine::update_agent_reported_inventory(&mut txn, &dpu_machine_id, &inventory).await?;

        txn.commit().await?;
    } else {
        return Err(
            CarbideError::InvalidArgument("inventory missing from request".to_string()).into(),
        );
    }

    tracing::debug!(
        machine_id = %dpu_machine_id,
        software_inventory = ?request.inventory,
        "update machine inventory",
    );

    Ok(Response::new(()))
}

pub(crate) async fn record_dpu_network_status(
    api: &Api,
    request: Request<rpc::DpuNetworkStatus>,
) -> Result<Response<()>, tonic::Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let dpu_machine_id =
        convert_and_log_machine_id::<DpuMachineId>(request.dpu_machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;

    // Load the DPU Object. We require it to update the health report based
    // on the last report
    let dpu_machine = db::machine::find_one(
        &mut txn,
        &dpu_machine_id,
        MachineSearchConfig {
            include_dpus: true,
            // We should probably be setting this to to true everywhere
            // or including FOR UPDATE on all SELECT queries, but
            // this wasn't being done up to now.  Based on the nature
            // of health/status reporting (things could go
            // unhealthy at any time, including moments after
            // checking), the locking probably wouldn't buy much
            // here, but maybe someone with broader knowledge of
            // the codebase should re-examine that assumption.
            for_update: false,
            ..Default::default()
        },
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "machine",
        id: dpu_machine_id.to_string(),
    })?;

    let machine_obs = {
        let mut obs = MachineNetworkStatusObservation::try_from(request.clone())
            .map_err(CarbideError::from)?;
        if let Some(agent_version) = obs.agent_version.as_ref() {
            obs.agent_version_superseded_at =
                db::carbide_version::date_superseded(&mut txn, agent_version.as_str()).await?;
        }
        obs
    };

    let any_observed_version_changed = match dpu_machine.network_status_observation.as_ref() {
        None => true,
        Some(old_observation) => old_observation.any_observed_version_changed(&machine_obs),
    };

    // Instance network observation is the part of network observation now.
    db::machine::update_network_status_observation(&mut txn, &dpu_machine_id, &machine_obs).await?;
    if dpu_machine.network_config.value.use_admin_network_changed == Some(true)
        && machine_obs.network_config_version.as_ref() == Some(&dpu_machine.network_config.version)
    {
        tracing::info!(
            dpu_machine_id = %dpu_machine_id,
            network_config_version = %dpu_machine.network_config.version,
            agent_version = ?machine_obs.agent_version,
            "Clearing use_admin_network_changed after matching-version ACK; OVS restart may have been skipped by agents that do not support the flag"
        );
        db::machine::clear_use_admin_network_changed_if_version_matches(
            &mut txn,
            &dpu_machine_id,
            &dpu_machine.network_config.version,
        )
        .await?;
    }
    tracing::trace!(
        machine_id = %dpu_machine_id,
        machine_network_config = ?request.network_config_version,
        instance_network_config = ?request.instance_network_config_version,
        instance_config_version = ?request.instance_config_version,
        agent_version = machine_obs.agent_version,
        "Applied network configs",
    );

    // Instance extension service observation is now separate from network observation.
    let extension_service_observation = request
        .dpu_extension_service_version
        .is_some()
        .then(|| {
            model::instance::status::extension_service::InstanceExtensionServiceStatusObservation::try_from(
                &request,
            )
        })
        .transpose()
        .map_err(CarbideError::from)?
        .map(|mut observation| {
            observation.retain_agent_managed_statuses();
            observation
        });
    if let Some(extension_service_observation) = &extension_service_observation {
        db::machine::update_extension_service_status_observation(
            &mut txn,
            &dpu_machine_id,
            model::extension_service::ExtensionServiceType::KubernetesPod,
            extension_service_observation,
        )
        .await?;
    }

    // Store the DPU submitted health-report
    let mut health_report = health_report::HealthReport::try_from(
        request
            .dpu_health
            .as_ref()
            .ok_or_else(|| CarbideError::MissingArgument("dpu_health"))?
            .clone(),
    )
    .map_err(|e| CarbideError::internal(e.to_string()))?;
    // We ignore what dpu-agent sends as timestamp and time, and replace
    // it with more accurate information
    health_report.source = health_report::HealthReport::DPU_AGENT_SOURCE.to_string();
    health_report.observed_at = Some(chrono::Utc::now());
    // Fix the in_alert times based on the previously stored report
    health_report.update_in_alert_since(dpu_machine.dpu_agent_health_report());

    db::machine::update_dpu_agent_health_report(&mut txn, &dpu_machine_id, &health_report).await?;

    for rpc::LastDhcpRequest {
        host_interface_id,
        timestamp,
    } in request.last_dhcp_requests.iter()
    {
        let Some(host_interface_id) = host_interface_id else {
            return Err(CarbideError::MissingArgument(
                "applied_config.last_dhcp_request.host_interface_id",
            )
            .into());
        };
        db::machine_interface::update_last_dhcp(
            &mut txn,
            *host_interface_id,
            Some(timestamp.parse().map_err(|e| {
                CarbideError::InvalidArgument(format!("failed parsing dhcp timestamp: {e}"))
            })?),
        )
        .await?;
    }

    txn.commit().await?;

    // Check if we need to flag this forge-dpu-agent for upgrade or mark an upgrade completed
    // We do this here because we just learnt about which version of forge-dpu-agent is
    // running.
    let mut txn = api.txn_begin().await?;

    if let Some(policy) = dpu_agent_upgrade_policy::get(&mut txn).await? {
        let snapshot =
            db::managed_host::load_snapshot(&mut txn, &dpu_machine_id, Default::default())
                .await?
                .ok_or(CarbideError::NotFoundError {
                    kind: "machine",
                    id: dpu_machine_id.to_string(),
                })?;

        let dpu_machine = snapshot
            .dpu_snapshots
            .iter()
            .find(|x| x.id == dpu_machine_id.into())
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "dpu",
                id: dpu_machine_id.to_string(),
            })?;

        if snapshot.host_snapshot.config.dpf.used_for_ingestion {
            // DPF-managed DPUs don't use this upgrade path. Clear any stale flag so the DPU
            // doesn't keep receiving upgrade signals after the host was switched to DPF.
            if dpu_machine.needs_agent_upgrade() {
                db::machine::set_dpu_agent_upgrade_requested(
                    &mut txn,
                    &dpu_machine_id,
                    false,
                    carbide_version::v!(build_version),
                )
                .await?;
            }
        } else {
            let _needs_upgrade =
                db::machine::apply_agent_upgrade_policy(&mut txn, policy, dpu_machine).await?;
        }
    }

    txn.commit().await?;

    if let Some(astra_config_status) = request.astra_config_status.as_ref() {
        process_astra_config_status(api, &dpu_machine_id, astra_config_status).await?;
    }

    // If this all worked and the DPU is healthy, we shouldn't emit a log line
    // If there is any error the report, the logging of the follow-up report is
    // suppressed for a certain amount of time to reduce logging noise.
    // The suppression is keyed by the type of errors that occur. If the set
    // of errors changed, the log will be emitted again.
    let suppress_log_key = match &request.network_config_error {
        Some(error) => error.to_string(),
        None => String::new(),
    };

    if suppress_log_key.is_empty()
        || !api
            .dpu_health_log_limiter
            .should_log(&dpu_machine_id, &suppress_log_key)
    {
        tracing::Span::current().record("logfmt.suppress", true);
    }

    // After everything else is done and the transaction is actually committed - wakeup
    // the host state handler to speed up reaction on the state change.
    // We only do this wakeup in case anything interesting changed to avoid the
    // state handler running unnecessarily.
    if any_observed_version_changed
        && let Err(err) = wakeup_host_state_handler_by_dpu_id(api, &dpu_machine_id).await
    {
        // The host machine could not even be looked up, so the identity in
        // hand is the reporting DPU rather than the host that stays asleep.
        carbide_instrument::emit(StateHandlerWakeupFailed {
            trigger: WakeupTrigger::DpuNetworkStatus,
            machine_id: dpu_machine_id.into(),
            err: err.to_string(),
        });
    }

    Ok(Response::new(()))
}

async fn wakeup_host_state_handler_by_dpu_id(
    api: &Api,
    dpu_machine_id: &DpuMachineId,
) -> Result<(), DatabaseError> {
    let host_machines_by_dpu_ids =
        db::machine::lookup_host_machine_ids_by_dpu_ids(&mut api.db_reader(), &[*dpu_machine_id])
            .await?;

    if let Some(host_machine_id) = host_machines_by_dpu_ids.get(dpu_machine_id)
        && let Err(err) = api
            .machine_state_handler_enqueuer
            .enqueue_object(host_machine_id)
            .await
    {
        carbide_instrument::emit(StateHandlerWakeupFailed {
            trigger: WakeupTrigger::DpuNetworkStatus,
            machine_id: host_machine_id.into(),
            err: err.to_string(),
        });
    }

    Ok(())
}

/// Network status of each managed host, as reported by forge-dpu-agent.
/// For use by forge-admin-cli
///
/// Currently: Status of HBN on each DPU
pub(crate) async fn get_all_managed_host_network_status(
    api: &Api,
    request: Request<rpc::ManagedHostNetworkStatusRequest>,
) -> Result<Response<rpc::ManagedHostNetworkStatusResponse>, Status> {
    log_request_data(&request);

    let all_status =
        db::machine::get_all_network_status_observation(&api.database_connection, 2000).await?;

    let mut out = Vec::with_capacity(all_status.len());
    for machine_network_status in all_status {
        out.push(machine_network_status.into());
    }
    Ok(Response::new(rpc::ManagedHostNetworkStatusResponse {
        all: out,
    }))
}

/// Should this DPU upgrade its forge-dpu-agent?
/// Once the upgrade is complete record_dpu_network_status will receive the updated
/// version and write the DB to say our upgrade is complete.
pub(crate) async fn dpu_agent_upgrade_check(
    api: &Api,
    request: tonic::Request<rpc::DpuAgentUpgradeCheckRequest>,
) -> Result<tonic::Response<rpc::DpuAgentUpgradeCheckResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let machine_id = MachineId::from_str(&req.machine_id).map_err(|_| {
        CarbideError::from(RpcDataConversionError::InvalidMachineId(
            req.machine_id.clone(),
        ))
    })?;
    log_machine_id(&machine_id);
    let dpu_machine_id = DpuMachineId::try_from(machine_id)
        .map_err(|error| CarbideError::InvalidArgument(error.to_string()))?;

    // We usually want these two to match
    let agent_version = req.current_agent_version;
    let server_version = carbide_version::v!(build_version);
    BuildVersion::try_from(server_version).map_err(|_| CarbideError::Internal {
        message: "Invalid server version, cannot check for upgrade".into(),
    })?;

    let mut txn = api.txn_begin().await?;

    let machine =
        db::machine::find_one(&mut txn, &dpu_machine_id, MachineSearchConfig::default()).await?;
    let machine = machine.ok_or(CarbideError::NotFoundError {
        kind: "dpu",
        id: machine_id.to_string(),
    })?;
    let should_upgrade = machine.needs_agent_upgrade();
    if should_upgrade {
        tracing::debug!(
            %machine_id,
            agent_version,
            server_version,
            "Needs forge-dpu-agent upgrade",
        );
    } else {
        tracing::trace!(%machine_id, agent_version, "forge-dpu-agent is up to date");
    }
    txn.commit().await?;

    // The debian/ubuntu package version is our build_version minus the initial `v`
    let package_version = &server_version[1..];

    let response = rpc::DpuAgentUpgradeCheckResponse {
        should_upgrade,
        package_version: package_version.to_string(),
        server_version: server_version.to_string(),
    };
    Ok(tonic::Response::new(response))
}

/// Get or set the forge-dpu-agent upgrade policy.
pub(crate) async fn dpu_agent_upgrade_policy_action(
    api: &Api,
    request: tonic::Request<rpc::DpuAgentUpgradePolicyRequest>,
) -> Result<tonic::Response<rpc::DpuAgentUpgradePolicyResponse>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let req = request.into_inner();
    let mut did_change = false;
    if let Some(new_policy) = req.new_policy {
        let policy: AgentUpgradePolicy = new_policy.rpc_into();

        dpu_agent_upgrade_policy::set(&mut txn, policy).await?;
        did_change = true;
    }

    let Some(active_policy) = dpu_agent_upgrade_policy::get(&mut txn).await? else {
        return Err(CarbideError::NotFoundError {
            kind: "agent_upgrade_policy",
            id: "active".to_string(),
        }
        .into());
    };
    txn.commit().await?;

    let response = rpc::DpuAgentUpgradePolicyResponse {
        active_policy: rpc::AgentUpgradePolicy::from(active_policy) as i32,
        did_change,
    };
    Ok(tonic::Response::new(response))
}

/// Returns whether adding a request for `machine_id` covers every attached DPU.
fn reprovision_request_covers_all_attached_dpus(
    snapshot: &ManagedHostStateSnapshot,
    machine_id: &MachineId,
) -> bool {
    if machine_id.machine_type().is_dpu() {
        snapshot
            .dpu_snapshots
            .iter()
            .all(|dpu| dpu.id == *machine_id || dpu.reprovision_requested.is_some())
    } else {
        snapshot.has_managed_dpus()
    }
}

/// Returns the DPUNode when this request may need the exact BF3 to GB200
/// deployment migration.
async fn dpf_deployment_migration_node(
    api: &Api,
    conn: &mut sqlx::PgConnection,
    snapshot: &ManagedHostStateSnapshot,
) -> Result<Option<String>, CarbideError> {
    if !api.runtime_config.dpf.enabled
        || !snapshot.host_snapshot.config.dpf.used_for_ingestion
        || snapshot.dpu_snapshots.is_empty()
    {
        return Ok(None);
    }

    let Some(rack_id) = snapshot.host_snapshot.rack_id.as_ref() else {
        return Ok(None);
    };
    let Some(rack) = db::rack::find_by(conn, ObjectColumnFilter::One(db::rack::IdColumn, rack_id))
        .await?
        .pop()
    else {
        return Ok(None);
    };
    let Some(rack_profile_id) = rack.rack_profile_id.as_ref() else {
        return Ok(None);
    };
    let Some(rack_profile) = api
        .runtime_config
        .rack_profiles
        .get(rack_profile_id.as_str())
    else {
        return Ok(None);
    };
    let every_dpu_uses_gb200_profile = snapshot.dpu_snapshots.iter().all(|dpu| {
        select_dpu_nvconfig_profile(
            rack_profile.product_family.as_ref(),
            dpu.status.hardware_info.as_ref(),
        )
        .is_some()
    });
    if !every_dpu_uses_gb200_profile {
        return Ok(None);
    }

    Ok(snapshot
        .host_snapshot
        .dpf_id()
        .map(|dpf_id| dpu_node_cr_name(&dpf_id)))
}

/// Returns whether the live DPUNode still belongs to the generic BF3
/// deployment and therefore needs a migration of the complete DPU set.
async fn dpf_deployment_migration_source_is_active(
    api: &Api,
    node_name: &str,
) -> Result<bool, CarbideError> {
    let dpf_sdk = api.dpf_sdk.as_deref().ok_or_else(|| {
        CarbideError::internal(
            "DPF SDK is unavailable while checking a DPF deployment migration".to_string(),
        )
    })?;
    if dpf_sdk
        .verify_node_labels(node_name, DpuDeploymentType::Bf3Gb200)
        .await
        .map_err(CarbideError::DpfError)?
    {
        return Ok(false);
    }

    dpf_sdk
        .verify_node_labels(node_name, DpuDeploymentType::Bf3)
        .await
        .map_err(CarbideError::DpfError)
}

/// Refuses a request change that would leave only part of the attached DPU set
/// selected while the shared DPUNode still needs deployment migration.
fn reject_partial_dpf_deployment_migration_request_set(
    snapshot: &ManagedHostStateSnapshot,
    machine_id: &MachineId,
    mode: rpc::dpu_reprovisioning_request::Mode,
    migration_source_is_active: bool,
) -> Result<(), CarbideError> {
    if !migration_source_is_active {
        return Ok(());
    }

    let requested_count = snapshot
        .dpu_snapshots
        .iter()
        .filter(|dpu| {
            let request_targets_dpu = machine_id.machine_type().is_host() || dpu.id == *machine_id;
            if request_targets_dpu {
                mode == rpc::dpu_reprovisioning_request::Mode::Set
            } else {
                dpu.reprovision_requested.is_some()
            }
        })
        .count();
    if requested_count == 0 || requested_count == snapshot.dpu_snapshots.len() {
        return Ok(());
    }

    let operation = match mode {
        rpc::dpu_reprovisioning_request::Mode::Set => "Set",
        rpc::dpu_reprovisioning_request::Mode::Clear => "Clear",
        rpc::dpu_reprovisioning_request::Mode::Restart => "Restart",
    };
    Err(CarbideError::FailedPrecondition(format!(
        "{operation} for DPU {machine_id} would leave only part of the attached DPU set selected \
         while the shared DPUNode still needs the GB200 deployment; submit {operation} with host \
         machine ID {} so every attached DPU changes together",
        snapshot.host_snapshot.id,
    )))
}

/// Refuses a reprovision request that would migrate a host to DPF while its
/// instance still has extension services attached.
///
/// Migrating flips the extension-service delivery path from the DPU agent to
/// DPUDevice placement labels, and an existing attachment cannot follow: it was
/// admitted against the agent path, and only a detach moves it. Reprovisioning a
/// strict subset of DPUs keeps the host on the legacy path and stays allowed.
///
/// `dpf_based_dpu_provisioning_possible` enforces the same invariant for
/// reprovisioning the state machine starts on its own; this exists so an
/// operator gets an actionable error instead of a silent legacy fallback.
fn reject_dpf_migration_that_would_strand_extension_services(
    api: &Api,
    snapshot: &ManagedHostStateSnapshot,
    machine_id: &MachineId,
) -> Result<(), CarbideError> {
    let host = &snapshot.host_snapshot;
    if host.config.dpf.used_for_ingestion
        || !host.config.dpf.enabled
        || !api.runtime_config.dpf.enabled
    {
        return Ok(());
    }

    let migrates_host_to_dpf = reprovision_request_covers_all_attached_dpus(snapshot, machine_id);

    let has_attached_extension_services = snapshot.instance.as_ref().is_some_and(|instance| {
        !instance
            .config
            .extension_services
            .service_configs
            .is_empty()
    });

    if migrates_host_to_dpf && has_attached_extension_services {
        return Err(CarbideError::FailedPrecondition(format!(
            "reprovisioning would migrate host {} to DPF, which cannot deliver the extension services attached to its instance; detach them first",
            host.id,
        )));
    }

    Ok(())
}

/// Validates request conditions that do not require a Kubernetes read.
fn validate_dpu_reprovisioning_request(
    api: &Api,
    snapshot: &ManagedHostStateSnapshot,
    machine_id: &MachineId,
    mode: rpc::dpu_reprovisioning_request::Mode,
) -> Result<(), CarbideError> {
    let update_alert = snapshot
        .aggregate_health
        .alerts
        .iter()
        .find(|alert| alert.id == *HOST_UPDATE_HEALTH_PROBE_ID);
    if !update_alert.is_some_and(|alert| {
        alert
            .classifications
            .contains(&health_report::HealthAlertClassification::prevent_allocations())
    }) {
        return Err(CarbideError::InvalidArgument(format!(
            "machine {machine_id} must have a 'HostUpdateInProgress' health alert with the \
             'PreventAllocations' classification before reprovisioning. set this precondition \
             with: `machine health-override add --template host-update <id>`",
        )));
    }

    let reprovisioning_started = snapshot.dpu_snapshots.iter().any(|dpu| {
        dpu.reprovision_requested
            .as_ref()
            .is_some_and(|request| request.started_at.is_some())
    });
    if reprovisioning_started && mode != rpc::dpu_reprovisioning_request::Mode::Restart {
        return Err(CarbideError::internal(
            "reprovisioning is already started".to_string(),
        ));
    }

    if mode == rpc::dpu_reprovisioning_request::Mode::Set {
        reject_dpf_migration_that_would_strand_extension_services(api, snapshot, machine_id)?;
    }

    Ok(())
}

/// Locks every attached DPU row in stable order before revalidating and
/// updating a reprovisioning request set.
async fn lock_attached_dpus(
    conn: &mut sqlx::PgConnection,
    snapshot: &ManagedHostStateSnapshot,
) -> Result<(), CarbideError> {
    let dpu_ids = snapshot
        .dpu_snapshots
        .iter()
        .map(|dpu| dpu.id)
        .collect::<Vec<_>>();
    let locked_dpus = db::machine::find(
        conn,
        db::ObjectFilter::List(&dpu_ids),
        MachineSearchConfig {
            for_update: true,
            ..MachineSearchConfig::default()
        },
    )
    .await?;
    if locked_dpus.len() != dpu_ids.len() {
        return Err(CarbideError::internal(format!(
            "expected to lock {} attached DPUs but found {}",
            dpu_ids.len(),
            locked_dpus.len(),
        )));
    }

    Ok(())
}

/// Loads the host snapshot used to validate and update DPU reprovisioning
/// requests.
async fn load_dpu_reprovisioning_snapshot(
    api: &Api,
    conn: &mut sqlx::PgConnection,
    machine_id: &MachineId,
) -> Result<ManagedHostStateSnapshot, CarbideError> {
    db::managed_host::load_snapshot(
        conn,
        machine_id,
        LoadSnapshotOptions {
            include_history: false,
            // The attached extension services checked by validation live on
            // the instance.
            include_instance_data: true,
            host_health_config: api.runtime_config.host_health,
        },
    )
    .await?
    .ok_or(CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })
}

/// Triggers DPU reprovisioning for one DPU or every DPU attached to a host.
///
/// `Set` and `Clear` intentionally keep the transaction that owns the admin
/// segment locks open across the DPF Kubernetes read. This prevents Site
/// Explorer from changing attachments between the authorizing snapshot and
/// the request writes.
#[allow(txn_held_across_await)]
pub(crate) async fn trigger_dpu_reprovisioning(
    api: &Api,
    request: tonic::Request<rpc::DpuReprovisioningRequest>,
) -> Result<tonic::Response<()>, tonic::Status> {
    use ::rpc::forge::dpu_reprovisioning_request::Mode;

    log_request_data(&request);
    let req = request.into_inner();
    let machine_id = req.machine_id.as_ref().or(req.dpu_id.as_ref());
    let machine_id = convert_and_log_machine_id::<MachineId>(machine_id)?;

    let mode = req.mode();
    // Set and Clear must choose their complete DPU set from the same attachment
    // state that authorizes the request writes. Take the Site Explorer lock
    // order before loading that snapshot so attachment changes cannot make a
    // previously ineligible host require migration after this check.
    let admin_lock_admission = if matches!(mode, Mode::Set | Mode::Clear) {
        Some(db::machine_interface::admin_lock_admission().await)
    } else {
        None
    };
    let mut txn = api.txn_begin().await?;
    if admin_lock_admission.is_some() {
        db::machine_interface::lock_all_admin_segments(txn.as_mut()).await?;
    }

    let mut snapshot = load_dpu_reprovisioning_snapshot(api, txn.as_mut(), &machine_id).await?;
    validate_dpu_reprovisioning_request(api, &snapshot, &machine_id, mode)?;

    let migration_node = if matches!(mode, Mode::Set | Mode::Clear) {
        dpf_deployment_migration_node(api, txn.as_mut(), &snapshot).await?
    } else {
        None
    };
    let migration_source_is_active = if let Some(node_name) = migration_node.as_deref() {
        // Keep the attachment locks through this Kubernetes read and the
        // request writes. Releasing them here would allow migration eligibility
        // to change between the two operations.
        tokio::time::timeout(
            DPF_DEPLOYMENT_LABEL_CHECK_TIMEOUT,
            dpf_deployment_migration_source_is_active(api, node_name),
        )
        .await
        .map_err(|_| {
            CarbideError::DpfError(carbide_dpf::DpfError::timeout(
                "DPUNode deployment label validation",
                format!(
                    "label checks for DPUNode '{node_name}' did not complete within {} seconds",
                    DPF_DEPLOYMENT_LABEL_CHECK_TIMEOUT.as_secs()
                ),
            ))
        })??
    } else {
        false
    };

    if mode == Mode::Set || migration_node.is_some() {
        // Set replaces the request JSON, and migration validation depends on
        // the complete request set. Lock and reload before either can write.
        lock_attached_dpus(txn.as_mut(), &snapshot).await?;
        snapshot = load_dpu_reprovisioning_snapshot(api, txn.as_mut(), &machine_id).await?;
        validate_dpu_reprovisioning_request(api, &snapshot, &machine_id, mode)?;
    }

    match mode {
        Mode::Set => {
            reject_partial_dpf_deployment_migration_request_set(
                &snapshot,
                &machine_id,
                mode,
                migration_source_is_active,
            )?;

            let initiator = req.initiator().as_str_name();
            if machine_id.machine_type().is_dpu() {
                db::machine::trigger_dpu_reprovisioning_request(
                    &machine_id,
                    &mut txn,
                    initiator,
                    req.update_firmware,
                )
                .await?;
            } else {
                for dpu_snapshot in &snapshot.dpu_snapshots {
                    db::machine::trigger_dpu_reprovisioning_request(
                        &dpu_snapshot.id,
                        &mut txn,
                        initiator,
                        req.update_firmware,
                    )
                    .await?;
                }
            }
        }
        Mode::Clear => {
            reject_partial_dpf_deployment_migration_request_set(
                &snapshot,
                &machine_id,
                mode,
                migration_source_is_active,
            )?;
            if machine_id.machine_type().is_dpu() {
                db::machine::clear_dpu_reprovisioning_request(&mut txn, &machine_id, true).await?;
            } else {
                for dpu_snapshot in &snapshot.dpu_snapshots {
                    db::machine::clear_dpu_reprovisioning_request(&mut txn, &dpu_snapshot.id, true)
                        .await?;
                }
            }
        }
        Mode::Restart => {
            // Restart case.
            // Restart is valid only for host_id.
            if !machine_id.machine_type().is_host() {
                return Err(CarbideError::InvalidArgument("A restart has to be triggered for all DPUs together. only host_id is accepted for restart operation".to_string()).into());
            }

            if !snapshot.has_managed_dpus() {
                return Err(CarbideError::InvalidArgument(
                    "machine has no DPUs, cannot trigger DPU reprovisioning".to_string(),
                )
                .into());
            }

            let ids = snapshot
                .dpu_snapshots
                .iter()
                .filter_map(|x| {
                    if x.reprovision_requested.is_some() {
                        Some(&x.id)
                    } else {
                        None
                    }
                })
                .collect_vec();

            if ids.is_empty() {
                return Err(CarbideError::InvalidArgument(
                    format!("no DPUs are currently reprovisioning on {machine_id}, cannot restart reprovisioning. use `set` to begin reprovisioning DPUs"),
                )
                    .into());
            }

            db::machine::restart_dpu_reprovisioning(&mut txn, &ids, req.update_firmware).await?;
        }
    }

    txn.commit().await?;
    drop(admin_lock_admission);

    Ok(Response::new(()))
}

// List DPUs waiting for reprovisioning
pub(crate) async fn list_dpu_waiting_for_reprovisioning(
    api: &Api,
    request: Request<rpc::DpuReprovisioningListRequest>,
) -> Result<Response<rpc::DpuReprovisioningListResponse>, Status> {
    log_request_data(&request);

    let dpus = db::machine::list_machines_requested_for_reprovisioning(&api.database_connection)
        .await?
        .into_iter()
        .map(
            |x| rpc::dpu_reprovisioning_list_response::DpuReprovisioningListItem {
                id: Some(x.id),
                state: x.current_state().to_string(),
                requested_at: x
                    .reprovision_requested
                    .as_ref()
                    .map(|a| a.requested_at.into()),
                initiator: x
                    .reprovision_requested
                    .as_ref()
                    .map(|a| a.initiator.clone())
                    .unwrap_or_default(),
                update_firmware: x
                    .reprovision_requested
                    .as_ref()
                    .map(|a| a.update_firmware)
                    .unwrap_or_default(),
                initiated_at: x
                    .reprovision_requested
                    .as_ref()
                    .map(|a| a.started_at.map(|x| x.into()))
                    .unwrap_or_default(),
                user_approval_received: x
                    .reprovision_requested
                    .as_ref()
                    .map(|x| x.user_approval_received)
                    .unwrap_or_default(),
            },
        )
        .collect_vec();

    Ok(Response::new(rpc::DpuReprovisioningListResponse { dpus }))
}

/// Get the configured BGP password.
async fn get_bgp_password(
    credential_reader: &dyn carbide_secrets::credentials::CredentialReader,
    credential_key: carbide_secrets::credentials::CredentialKey,
) -> Result<String, CarbideError> {
    let credential = credential_reader
        .get_credentials(&credential_key)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("Could not find the credential: {}", e),
        })?;

    Ok(match credential {
        Some(Credentials::UsernamePassword { password, .. }) => password,
        _ => {
            return Err(CarbideError::Internal {
                message: "Could not find BGP credential".to_string(),
            });
        }
    })
}

#[cfg(test)]
mod deny_prefix_tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn prefixes_follow_the_effective_virtualizer() {
        let prefixes = ["192.0.2.0/24", "2001:db8::/32"].map(|prefix| prefix.parse().unwrap());
        let ipv4_only = vec!["192.0.2.0/24".to_string()];

        value_scenarios!(
            run = |virtualization_type| {
                deny_prefixes_for_agent(&prefixes, virtualization_type)
            };
            "dual-stack agent policy" {
                VpcVirtualizationType::Fnn => vec![
                    "192.0.2.0/24".to_string(),
                    "2001:db8::/32".to_string(),
                ],
            }

            "non-FNN wire compatibility" {
                VpcVirtualizationType::EthernetVirtualizer => ipv4_only.clone(),
                VpcVirtualizationType::EthernetVirtualizerWithNvue => ipv4_only.clone(),
                VpcVirtualizationType::Flat => ipv4_only,
            }
        );
    }

    #[test]
    fn deprecated_field_filters_dual_stack_site_fabric() {
        let deny_prefixes = vec!["198.51.100.0/24".to_string()];
        let site_fabric_prefixes =
            ["192.0.2.0/24", "2001:db8::/32"].map(|prefix| prefix.parse().unwrap());
        let fnn_prefixes = vec![
            "192.0.2.0/24".to_string(),
            "2001:db8::/32".to_string(),
            "198.51.100.0/24".to_string(),
        ];
        let ipv4_only = vec!["192.0.2.0/24".to_string(), "198.51.100.0/24".to_string()];

        value_scenarios!(
            run = |virtualization_type| deprecated_deny_prefixes_for_agent(
                &deny_prefixes,
                &site_fabric_prefixes,
                VpcIsolationBehaviorType::MutualIsolation,
                virtualization_type,
            );
            "dual-stack FNN compatibility field" {
                VpcVirtualizationType::Fnn => fnn_prefixes,
            }

            "IPv4-only compatibility field" {
                VpcVirtualizationType::EthernetVirtualizer => ipv4_only.clone(),
                VpcVirtualizationType::EthernetVirtualizerWithNvue => ipv4_only.clone(),
                VpcVirtualizationType::Flat => ipv4_only,
            }
        );
    }
}

#[cfg(test)]
mod tenant_fqdn_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    #[test]
    fn physical_ip_selection_is_stable_and_prefers_ipv4() {
        let ipv4_low: IpAddr = "192.0.2.10".parse().unwrap();
        let ipv4_high: IpAddr = "192.0.2.20".parse().unwrap();
        let ipv6_low: IpAddr = "2001:db8::10".parse().unwrap();
        let ipv6_high: IpAddr = "2001:db8::20".parse().unwrap();

        value_scenarios!(
            run = preferred_physical_ip;
            "no physical address" {
                vec![] => None,
            }

            "one address family" {
                vec![ipv4_high, ipv4_low] => Some(ipv4_low),
                vec![ipv6_high, ipv6_low] => Some(ipv6_low),
            }

            "dual-stack address order" {
                vec![ipv6_low, ipv4_high, ipv6_high, ipv4_low] => Some(ipv4_low),
                vec![ipv4_low, ipv6_high, ipv4_high, ipv6_low] => Some(ipv4_low),
            }
        );
    }

    #[test]
    fn tenant_fqdn_uses_the_configured_name_or_physical_address() {
        struct FqdnCase {
            tenant_hostname: Option<&'static str>,
            physical_ip: Option<IpAddr>,
        }

        let ipv4: IpAddr = "192.0.2.10".parse().unwrap();
        let ipv6: IpAddr = "2001:db8::10".parse().unwrap();
        let instance_id = InstanceId::new();

        scenarios!(
            run = |FqdnCase {
                tenant_hostname,
                physical_ip,
            }| tenant_interface_fqdn(
                tenant_hostname,
                physical_ip.as_ref(),
                &instance_id,
                "tenant.example",
            ).map_err(drop);
            "tenant-supplied hostname takes precedence over the address" {
                FqdnCase {
                    tenant_hostname: Some("customer-host"),
                    physical_ip: Some(ipv6),
                } => Yields("customer-host.tenant.example".to_string()),
                FqdnCase {
                    tenant_hostname: Some("customer-host"),
                    physical_ip: None,
                } => Yields("customer-host.tenant.example".to_string()),
            }

            "address-derived hostname" {
                FqdnCase {
                    tenant_hostname: None,
                    physical_ip: Some(ipv4),
                } => Yields("192-0-2-10.tenant.example".to_string()),
                FqdnCase {
                    tenant_hostname: None,
                    physical_ip: Some(ipv6),
                } => Yields(concat!(
                    "2001-0db8-0000-0000-0000-0000-0000-0010",
                    ".tenant.example"
                )
                .to_string()),
            }

            "stable instance fallback without a hostname or address" {
                FqdnCase {
                    tenant_hostname: None,
                    physical_ip: None,
                } => Yields(format!("{instance_id}.tenant.example")),
            }
        );
    }
}

#[cfg(test)]
mod consolidated_network_config_tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use carbide_test_support::value_scenarios;
    use model::machine::network::{
        ManagedHostNetworkConfig, ManagedHostQuarantineMode, ManagedHostQuarantineState,
    };

    use super::*;

    fn dpu_ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
    }

    // The DPU layer contributes loopback_ip; an empty host layer leaves
    // quarantine_state absent.
    #[test]
    fn dpu_loopback_ip_carries_through_with_empty_host_layer() {
        let host = ManagedHostNetworkConfig::default();
        let consolidated = build_consolidated_network_config(
            &host,
            dpu_ip(),
            None,
            VpcVirtualizationType::EthernetVirtualizer,
        );
        assert_eq!(consolidated.loopback_ip, "10.0.0.1");
        assert!(consolidated.quarantine_state.is_none());
    }

    // The host layer contributes quarantine_state when set; the DPU layer
    // still owns loopback_ip independently.
    #[test]
    fn host_quarantine_state_carries_through_alongside_dpu_loopback() {
        let host = ManagedHostNetworkConfig {
            quarantine_state: Some(ManagedHostQuarantineState {
                reason: Some("test".to_string()),
                mode: ManagedHostQuarantineMode::BlockAllTraffic,
            }),
            ..ManagedHostNetworkConfig::default()
        };
        let consolidated = build_consolidated_network_config(
            &host,
            dpu_ip(),
            None,
            VpcVirtualizationType::EthernetVirtualizer,
        );
        assert_eq!(consolidated.loopback_ip, "10.0.0.1");
        let qs = consolidated.quarantine_state.expect("quarantine_state");
        assert_eq!(qs.reason.as_deref(), Some("test"));
    }

    // Host-layer fields that aren't part of the consolidated proto shape
    // (loopback_ip on the host and use_admin_network) do NOT leak into the
    // response -- the consolidator deliberately picks
    // only quarantine_state from the host layer. Catches accidental changes
    // to that contract.
    #[test]
    fn host_layer_fields_outside_the_consolidated_shape_are_ignored() {
        let host = ManagedHostNetworkConfig {
            // loopback_ip on the host's row is meaningless and shouldn't
            // be served to the DPU agent -- the DPU's own loopback_ip
            // (passed separately) is what matters.
            loopback_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 99))),
            loopback_ip_v6: None,
            // The host-level use_admin_network is reported in a separate
            // top-level response field, not in this consolidated struct.
            use_admin_network: Some(false),
            quarantine_state: None,
            use_admin_network_changed: None,
        };
        let consolidated = build_consolidated_network_config(
            &host,
            dpu_ip(),
            None,
            VpcVirtualizationType::EthernetVirtualizer,
        );
        assert_eq!(
            consolidated.loopback_ip, "10.0.0.1",
            "consolidator must use the dpu_loopback_ip arg, not host.loopback_ip"
        );
        assert!(consolidated.quarantine_state.is_none());
    }

    #[test]
    fn dpu_ipv6_loopback_carries_through_independently() {
        let host_ip = Ipv6Addr::new(0x2001, 0xdb8, 0xffff, 0, 0, 0, 0, 1);
        let first_dpu_ip = Ipv6Addr::new(0x2001, 0xdb8, 0x2390, 0, 0, 0, 0, 1);
        let second_dpu_ip = Ipv6Addr::new(0x2001, 0xdb8, 0x2390, 0, 0, 0, 0, 2);

        value_scenarios!(
            run = |(host_loopback_ip_v6, dpu_loopback_ip_v6, virtualization_type)| {
                let host = ManagedHostNetworkConfig {
                    loopback_ip_v6: host_loopback_ip_v6,
                    ..ManagedHostNetworkConfig::default()
                };
                build_consolidated_network_config(
                    &host,
                    dpu_ip(),
                    dpu_loopback_ip_v6,
                    virtualization_type,
                )
                .loopback_ip_v6
            };
            "FNN uses the requesting DPU's IPv6 loopback" {
                (None, None, VpcVirtualizationType::Fnn) => None,
                (Some(host_ip), None, VpcVirtualizationType::Fnn) => None,
                (None, Some(first_dpu_ip), VpcVirtualizationType::Fnn) => {
                    Some(first_dpu_ip.to_string())
                },
                (Some(host_ip), Some(second_dpu_ip), VpcVirtualizationType::Fnn) => {
                    Some(second_dpu_ip.to_string())
                },
            }

            "non-FNN agents keep the IPv4-only wire contract" {
                (
                    None,
                    Some(first_dpu_ip),
                    VpcVirtualizationType::EthernetVirtualizer,
                ) => None,
                (
                    None,
                    Some(first_dpu_ip),
                    VpcVirtualizationType::EthernetVirtualizerWithNvue,
                ) => None,
                (None, Some(first_dpu_ip), VpcVirtualizationType::Flat) => None,
            }
        );
    }
}
