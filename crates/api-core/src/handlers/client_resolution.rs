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
use std::net::IpAddr;

use ::rpc::forge as rpc;
use carbide_uuid::machine::{DpuMachineId, HostMachineId, MachineIdSubtypeTrait};
use carbide_uuid::network::NetworkSegmentId;
use db::ObjectColumnFilter;
use db::db_read::DbReader;
use model::instance::config::network::{
    DeviceLocator, InstanceNetworkConfig, InstanceNetworkConfigUpdate, InterfaceFunctionId,
};
use model::instance::config::tenant_config::HOSTNAME_RE;
use model::instance::snapshot::InstanceSnapshot;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{InstanceState, Machine, MachineInterfaceSnapshot, ManagedHostState};
use model::machine_interface::InterfaceType;
use model::network_segment::NetworkSegmentType;
use model::rack_type::{RackProductFamily, select_dpu_nvconfig_profile};
use sqlx::PgConnection;

use crate::CarbideError;
use crate::api::Api;

/// What `resolve_client_ip` returned. Either the IP belongs directly
/// to a `machine_interface` (admin / host case), or it belongs to a
/// tenant-allocated `instance_address` (instance case).
enum ResolvedClient {
    MachineInterface(Box<MachineInterfaceSnapshot>),
    Instance(Box<OverlayAddressOwner>),
}

impl From<MachineInterfaceSnapshot> for ResolvedClient {
    fn from(snapshot: MachineInterfaceSnapshot) -> Self {
        ResolvedClient::MachineInterface(Box::new(snapshot))
    }
}

enum PreferredLookup {
    MachineInterface,
    Instance,
}

/// One unambiguous overlay owner and the segments on which it holds the
/// observed address.
pub(super) struct OverlayAddressOwner {
    instance: InstanceSnapshot,
    segment_ids: HashSet<NetworkSegmentId>,
}

/// A configured function and its placement. Function identity keeps a PF and
/// VF on the same DPU distinct when retained configurations are compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct InstanceAttachment<'a> {
    function_id: &'a InterfaceFunctionId,
    device_locator: Option<&'a DeviceLocator>,
}

fn matching_instance_attachments<'a>(
    client_ip: IpAddr,
    segment_ids: &HashSet<NetworkSegmentId>,
    network: &'a InstanceNetworkConfig,
    update: Option<&'a InstanceNetworkConfigUpdate>,
) -> HashSet<InstanceAttachment<'a>> {
    // Both sides of a network update can own addresses until old resources
    // are released. Match the live segment too, since VPCs can reuse an IP.
    std::iter::once(network)
        .chain(
            update
                .into_iter()
                .flat_map(|update| [&update.old_config, &update.new_config]),
        )
        .flat_map(|config| &config.interfaces)
        .filter(|interface| {
            interface
                .network_segment_id
                .is_some_and(|segment| segment_ids.contains(&segment))
                && interface.ip_addrs.values().any(|ip| *ip == client_ip)
        })
        .map(|interface| InstanceAttachment {
            function_id: &interface.function_id,
            device_locator: interface.device_locator.as_ref(),
        })
        .collect()
}

/// The overlay ownership states an IP-only caller needs to distinguish.
pub(super) enum OverlayAddressOwnerLookup {
    NotFound,
    One(Box<OverlayAddressOwner>),
    Ambiguous,
}

/// `find_overlay_address_owner` returns one overlay owner only when the
/// address identifies it without any VPC context. Callers that use the result
/// to authorize a request or select tenant data must keep the full decision in
/// one transaction, so allocation cannot add another owner between reads.
pub(super) async fn find_overlay_address_owner<DB>(
    conn: &mut DB,
    client_ip: IpAddr,
) -> Result<OverlayAddressOwnerLookup, CarbideError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let addresses = db::instance_address::find_all_by_address(&mut *conn, client_ip).await?;
    let Some(first_address) = addresses.first() else {
        return Ok(OverlayAddressOwnerLookup::NotFound);
    };
    let instance_id = first_address.instance_id;

    if addresses
        .iter()
        .any(|address| address.instance_id != instance_id)
    {
        let instance_ids = addresses
            .iter()
            .map(|address| address.instance_id)
            .collect::<HashSet<_>>();
        tracing::warn!(
            %client_ip,
            instance_ids = ?instance_ids,
            "client IP matches more than one overlay instance"
        );
        return Ok(OverlayAddressOwnerLookup::Ambiguous);
    }

    let Some(instance) = db::instance::find_by_id(&mut *conn, instance_id).await? else {
        tracing::warn!(
            %client_ip,
            %instance_id,
            "overlay address owner disappeared during client resolution"
        );
        return Err(CarbideError::FailedPrecondition(format!(
            "client IP {client_ip} ownership changed during resolution"
        )));
    };
    let segment_ids = addresses
        .into_iter()
        .map(|address| address.segment_id)
        .collect();
    Ok(OverlayAddressOwnerLookup::One(Box::new(
        OverlayAddressOwner {
            instance,
            segment_ids,
        },
    )))
}

/// Returns whether both rows describe the same HostInband interface. Matching
/// only the machine ID is not enough: the interface must also be a Data
/// interface on the exact HostInband segment that owns the overlay address.
/// A snapshot without its denormalized network-segment type is treated as not
/// matching; both current callers populate it.
pub(super) fn is_same_host_inband_interface(
    machine_interface: &MachineInterfaceSnapshot,
    owner: &OverlayAddressOwner,
) -> bool {
    machine_interface.machine_id == Some(owner.instance.machine_id.into())
        && machine_interface.interface_type == InterfaceType::Data
        && machine_interface.network_segment_type == Some(NetworkSegmentType::HostInband)
        && owner.segment_ids.contains(&machine_interface.segment_id)
}

/// Resolve a client IP to a [`ResolvedClient`].
///
/// A direct machine-interface address and one overlay instance may represent
/// the same physical HostInband interface. `preferred_lookup` decides which
/// view of that host the caller needs. Unrelated owners are ambiguous and must
/// be rejected rather than using the preference to select one.
async fn resolve_client_ip(
    conn: &mut PgConnection,
    client_ip: IpAddr,
    preferred_lookup: PreferredLookup,
) -> Result<ResolvedClient, CarbideError> {
    let machine_interface = db::machine_interface::find_by_ip(&mut *conn, client_ip).await?;
    let overlay_owner = find_overlay_address_owner(&mut *conn, client_ip).await?;

    match (machine_interface, overlay_owner) {
        (None, OverlayAddressOwnerLookup::NotFound) => Err(CarbideError::NotFoundError {
            kind: "Client",
            id: client_ip.to_string(),
        }),
        (_, OverlayAddressOwnerLookup::Ambiguous) => Err(CarbideError::FailedPrecondition(
            format!("client IP {client_ip} matches more than one overlay instance"),
        )),
        (Some(machine_interface), OverlayAddressOwnerLookup::NotFound) => {
            Ok(machine_interface.into())
        }
        (None, OverlayAddressOwnerLookup::One(owner)) => Ok(ResolvedClient::Instance(owner)),
        (Some(machine_interface), OverlayAddressOwnerLookup::One(owner))
            if is_same_host_inband_interface(&machine_interface, &owner) =>
        {
            Ok(match preferred_lookup {
                PreferredLookup::MachineInterface => machine_interface.into(),
                PreferredLookup::Instance => ResolvedClient::Instance(owner),
            })
        }
        (Some(machine_interface), OverlayAddressOwnerLookup::One(owner)) => {
            tracing::warn!(
                %client_ip,
                machine_interface_id = %machine_interface.id,
                instance_id = %owner.instance.id,
                "client IP matches unrelated underlay and overlay owners"
            );
            Err(CarbideError::FailedPrecondition(format!(
                "client IP {client_ip} matches unrelated underlay and overlay owners"
            )))
        }
    }
}

/// Resolve a client IP to the host's `machine_interface` for PXE-script
/// generation and discovery cloud-init. For direct-interface IPs this returns
/// the matching interface. For tenant-allocated IPs it resolves through the
/// instance's configured physical attachment. Retained configurations must
/// agree on the function and attachment. VF requests and addresses without a
/// matching config use compatibility selection: primary, then an Admin-segment
/// interface, then the first interface. Omitted placement uses this fallback
/// when the host has no primary interface.
pub(super) async fn resolve_machine_interface(
    conn: &mut PgConnection,
    client_ip: IpAddr,
) -> Result<MachineInterfaceSnapshot, CarbideError> {
    match resolve_client_ip(conn, client_ip, PreferredLookup::MachineInterface).await? {
        ResolvedClient::MachineInterface(iface) => Ok(*iface),
        ResolvedClient::Instance(owner) => {
            let instance = &owner.instance;
            let interfaces_by_machine =
                db::machine_interface::find_by_machine_ids(&mut *conn, &[instance.machine_id])
                    .await?;
            let host_interfaces =
                interfaces_by_machine
                    .get(&instance.machine_id)
                    .ok_or_else(|| {
                        CarbideError::internal(format!(
                            "no machine_interfaces for host {}",
                            instance.machine_id,
                        ))
                    })?;

            let attachments = matching_instance_attachments(
                client_ip,
                &owner.segment_ids,
                &instance.config.network,
                instance.update_network_config_request.as_ref(),
            );
            let needs_physical_resolution = attachments.len() > 1
                || attachments.iter().any(|attachment| {
                    matches!(attachment.function_id, InterfaceFunctionId::Physical {})
                });
            let dpu_ids_by_device = if needs_physical_resolution
                && attachments
                    .iter()
                    .any(|attachment| attachment.device_locator.is_some())
            {
                let host = db::machine::find_one(
                    &mut *conn,
                    &instance.machine_id,
                    MachineSearchConfig::default(),
                )
                .await?
                .ok_or_else(|| {
                    CarbideError::internal(format!(
                        "host {} disappeared during client resolution",
                        instance.machine_id
                    ))
                })?;
                host.get_dpu_device_and_id_mappings()?.1
            } else {
                HashMap::new()
            };
            if let Some(interface) = select_instance_attachment(
                client_ip,
                &attachments,
                host_interfaces,
                &dpu_ids_by_device,
            )? {
                return Ok(interface.clone());
            }

            let admin_segment_ids: HashSet<_> = db::network_segment::admin(&mut *conn)
                .await?
                .into_iter()
                .map(|s| s.id)
                .collect();

            // Keep compatibility selection when no configured PF was selected.
            host_interfaces
                .iter()
                .find(|i| i.primary_interface)
                .or_else(|| {
                    host_interfaces
                        .iter()
                        .find(|i| admin_segment_ids.contains(&i.segment_id))
                })
                .or_else(|| host_interfaces.first())
                .cloned()
                .ok_or_else(|| {
                    CarbideError::internal(format!(
                        "host {} has no machine_interfaces",
                        instance.machine_id,
                    ))
                })
        }
    }
}

/// Returns the configured PF's host interface when retained descriptions agree.
/// Conflicting descriptions or unresolved explicit placement fail. No matching
/// config, agreeing VFs, or omitted placement without a primary return `None`
/// for compatibility selection; a sole VF needs no placement resolution.
fn select_instance_attachment<'a>(
    client_ip: IpAddr,
    attachments: &HashSet<InstanceAttachment<'_>>,
    host_interfaces: &'a [MachineInterfaceSnapshot],
    dpu_ids_by_device: &HashMap<String, Vec<DpuMachineId>>,
) -> Result<Option<&'a MachineInterfaceSnapshot>, CarbideError> {
    let Some(first) = attachments.iter().next() else {
        return Ok(None);
    };
    if attachments.len() == 1 && matches!(first.function_id, InterfaceFunctionId::Virtual { .. }) {
        return Ok(None);
    }

    let primary = host_interfaces
        .iter()
        .find(|interface| interface.primary_interface);
    let mut selected: Option<(&InterfaceFunctionId, Option<&MachineInterfaceSnapshot>)> = None;
    for attachment in attachments {
        let interface = match attachment.device_locator {
            None => primary,
            Some(locator) => {
                let dpu_id = dpu_ids_by_device
                    .get(&locator.device)
                    .and_then(|ids| ids.get(locator.device_instance))
                    .ok_or_else(|| {
                        CarbideError::FailedPrecondition(format!(
                            "client IP {client_ip} has no managed DPU for device locator {locator}"
                        ))
                    })?;
                let mut interfaces = host_interfaces.iter().filter(|interface| {
                    interface.interface_type == InterfaceType::Data
                        && interface.attached_dpu_machine_id == Some(*dpu_id)
                });
                let interface = interfaces.next().ok_or_else(|| {
                    CarbideError::FailedPrecondition(format!(
                        "client IP {client_ip} has no host interface for device locator {locator}"
                    ))
                })?;
                if interfaces.next().is_some() {
                    return Err(CarbideError::FailedPrecondition(format!(
                        "client IP {client_ip} has multiple host interfaces for device locator {locator}"
                    )));
                }
                Some(interface)
            }
        };
        if let Some((function_id, selected_interface)) = selected
            && (function_id != attachment.function_id
                || selected_interface.map(|interface| interface.id)
                    != interface.map(|interface| interface.id))
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "client IP {client_ip} has conflicting retained interface configurations"
            )));
        }
        selected = Some((attachment.function_id, interface));
    }

    match selected {
        Some((InterfaceFunctionId::Physical {}, interface)) => Ok(interface),
        _ => Ok(None),
    }
}

/// Returns the product family reported by Site Explorer, or falls back to the
/// configured rack profile when that report does not name a known family.
async fn resolve_host_product_family(
    api: &Api,
    conn: &mut PgConnection,
    host: &Machine<impl MachineIdSubtypeTrait>,
) -> Result<Option<RackProductFamily>, CarbideError> {
    if let Some(host_bmc_ip) = host.status.bmc_info.ip {
        let endpoints = db::explored_endpoints::find_by_ips(&mut *conn, vec![host_bmc_ip]).await?;
        if let Some(product_family) = endpoints
            .first()
            .and_then(|endpoint| endpoint.report.model())
            .as_deref()
            .and_then(RackProductFamily::from_hardware_model)
        {
            return Ok(Some(product_family));
        }
    }

    let Some(rack_id) = host.rack_id.as_ref() else {
        return Ok(None);
    };
    let Some(rack) = db::rack::find_by(
        &mut *conn,
        ObjectColumnFilter::One(db::rack::IdColumn, rack_id),
    )
    .await?
    .pop() else {
        return Ok(None);
    };
    let Some(rack_profile_id) = rack.rack_profile_id.as_ref() else {
        return Ok(None);
    };

    Ok(api
        .runtime_config
        .rack_profiles
        .get(rack_profile_id.as_str())
        .and_then(|profile| profile.product_family.clone()))
}

/// Selects a DPU profile from the attached host's observed or configured
/// product family and the DPU's hardware identity. Missing relationships mean
/// no platform profile applies; database failures still propagate.
async fn resolve_dpu_nvconfig_profile(
    api: &Api,
    conn: &mut PgConnection,
    machine_interface: &MachineInterfaceSnapshot,
) -> Result<Option<rpc::DpuNvConfigProfile>, CarbideError> {
    let Some(dpu_machine_id) = machine_interface.machine_id.as_ref() else {
        return Ok(None);
    };
    let Ok(dpu_machine_id) = DpuMachineId::try_from(*dpu_machine_id) else {
        return Ok(None);
    };

    let Some(host) = db::machine::find_host_by_dpu_machine_id(&mut *conn, &dpu_machine_id).await?
    else {
        return Ok(None);
    };
    let product_family = resolve_host_product_family(api, conn, &host).await?;

    let Some(dpu) =
        db::machine::find_one(&mut *conn, &dpu_machine_id, MachineSearchConfig::default()).await?
    else {
        return Ok(None);
    };

    Ok(
        select_dpu_nvconfig_profile(product_family.as_ref(), dpu.status.hardware_info.as_ref())
            .map(rpc::DpuNvConfigProfile::from),
    )
}

/// Resolve a client IP to its `CloudInitInstructions` response. The
/// interface arm produces a discovery-instructions response (for
/// unassigned hosts running scout, etc.); the instance arm produces an
/// instance-specific response with the tenant-provided user_data.
pub(super) async fn resolve_cloud_init_instructions(
    api: &Api,
    conn: &mut PgConnection,
    client_ip: IpAddr,
) -> Result<rpc::CloudInitInstructions, CarbideError> {
    let cloud_name = "nvidia".to_string();
    let platform = "forge".to_string();

    let resolved_ip = {
        let mut resolved = resolve_client_ip(conn, client_ip, PreferredLookup::Instance).await?;

        // Is this an instance IP? If so, we use its cloud-init config *only* if it's Assigned/Ready.
        if let ResolvedClient::Instance(owner) = &resolved
            && let Some(managed_host_state) =
                db::machine::lookup_managed_host_state(&mut *conn, owner.instance.machine_id.into())
                    .await?
        {
            let is_assigned_and_ready = matches!(
                managed_host_state,
                ManagedHostState::Assigned {
                    instance_state: InstanceState::Ready | InstanceState::WaitingForRebootToReady,
                }
            );

            if !is_assigned_and_ready {
                tracing::info!(
                    instance_id=%owner.instance.id,
                    machine_id=%owner.instance.machine_id,
                    managed_host_state = %managed_host_state,
                    "cloud-init instructions: machine is not Assigned/Ready, using discovery cloud-init"
                );
                let machine_interface = resolve_machine_interface(&mut *conn, client_ip).await?;
                resolved = ResolvedClient::MachineInterface(Box::new(machine_interface));
            }
        }

        resolved
    };

    match resolved_ip {
        ResolvedClient::Instance(owner) => {
            let instance = owner.instance;
            let local_hostname = if HOSTNAME_RE.is_match(&instance.metadata.name) {
                Some(instance.metadata.name.clone())
            } else {
                tracing::warn!(
                    instance_id=%instance.id,
                    instance_name=%instance.metadata.name,
                    "instance name is not a valid hostname; omitting local-hostname from cloud-init meta-data"
                );
                None
            };

            Ok(rpc::CloudInitInstructions {
                custom_cloud_init: instance.config.os.user_data,
                discovery_instructions: None,
                metadata: Some(rpc::CloudInitMetaData {
                    instance_id: instance.id.to_string(),
                    cloud_name,
                    platform,
                    local_hostname,
                }),
                api_url_override: None,
                pxe_url_override: None,
            })
        }
        ResolvedClient::MachineInterface(machine_interface) => {
            let domain = match machine_interface.domain_id {
                Some(domain_id) => Some(
                    db::dns::domain::find_by_uuid(&mut *conn, domain_id)
                        .await?
                        .ok_or_else(|| {
                            CarbideError::internal(format!(
                                "could not find domain with id {domain_id}"
                            ))
                        })?,
                ),
                // Host Data interfaces, including predicted hosts, use machine
                // identity for Scout metadata and need no domain. This includes
                // secondary interfaces booting over tenant networking on release.
                None if machine_interface.interface_type == InterfaceType::Data
                    && machine_interface
                        .machine_id
                        .is_some_and(|id| HostMachineId::try_from(id).is_ok()) =>
                {
                    None
                }
                None => {
                    return Err(CarbideError::internal(format!(
                        "machine interface did not have an associated domain {}",
                        machine_interface.id
                    )));
                }
            };

            // This custom pxe is different from a customer instance of pxe. It is more for testing
            // one off changes until a real dev env is established and we can just override our
            // existing code to test. It is possible for the user data to be null if we are only
            // trying to test the pxe, and this will follow the same code path and retrieve the
            // non custom user data.
            let custom_cloud_init =
                match db::machine_boot_override::find_optional(&mut *conn, machine_interface.id)
                    .await?
                {
                    Some(machine_boot_override) => machine_boot_override.custom_user_data,
                    None => None,
                };

            let metadata: Option<rpc::CloudInitMetaData> = machine_interface
                .machine_id
                .as_ref()
                .map(|machine_id| rpc::CloudInitMetaData {
                    instance_id: machine_id.to_string(),
                    cloud_name,
                    platform,
                    local_hostname: None,
                });

            // For interfaces on the static-assignments segment, include
            // hostname or IP-based URL overrides so external hosts can
            // reach carbide-api and carbide-pxe services. Just to reiterate,
            // these can be either routable IPs, or externally resolvable
            // hostnames to routable IPs.
            let is_external = machine_interface.segment_id
                == db::network_segment::static_assignments(&mut *conn)
                    .await
                    .map(|s| s.id)
                    .unwrap_or_default();

            let (api_url_override, pxe_url_override) = if is_external {
                (
                    api.runtime_config.external_api_url.clone(),
                    api.runtime_config.external_pxe_url.clone(),
                )
            } else {
                (None, None)
            };

            let vmaas_config = api.runtime_config.vmaas_config.as_ref();
            let host_representor_bridging =
                vmaas_config.and_then(|config| config.bridging.as_ref());
            let dpu_nvconfig_profile = resolve_dpu_nvconfig_profile(api, conn, &machine_interface)
                .await?
                .unwrap_or(rpc::DpuNvConfigProfile::Unspecified)
                as i32;

            Ok(rpc::CloudInitInstructions {
                custom_cloud_init,
                discovery_instructions: Some(rpc::CloudInitDiscoveryInstructions {
                    machine_interface: Some((*machine_interface).into()),
                    domain: domain.map(|domain| rpc::PxeDomain {
                        domain: Some(rpc::pxe_domain::Domain::NewDomain(domain.into())),
                    }),
                    hbn_reps: vmaas_config.and_then(|config| config.hbn_reps.clone()),
                    num_of_vfs: Some(api.runtime_config.dpu_config.num_of_vfs),
                    hbn_bridge: host_representor_bridging.map(|config| config.hbn_bridge.clone()),
                    host_representor_intercept_bridging: host_representor_bridging
                        .and_then(|b| b.host_representor_intercept_bridging_provisioning_config()),
                    bootstrap_ca_source: rpc::BootstrapCaSource::from(
                        api.runtime_config.dpu_config.bootstrap_ca_source,
                    ) as i32,
                    dpu_nvconfig_profile,
                }),
                metadata,
                api_url_override,
                pxe_url_override,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;
    use carbide_uuid::machine::MachineInterfaceId;
    use mac_address::MacAddress;
    use model::test_support::machine_snapshot::{dpu_machine_id, host_machine_id};

    use super::*;

    fn host_interface(index: u8) -> MachineInterfaceSnapshot {
        let mut interface =
            MachineInterfaceSnapshot::mock_with_mac(MacAddress::new([0x02, 0, 0, 0, 0, index]));
        interface.id = MachineInterfaceId::from(uuid::Uuid::from_u128(index.into()));
        interface.machine_id = Some(host_machine_id().into());
        interface.attached_dpu_machine_id = Some(dpu_machine_id(index));
        interface.primary_interface = index == 0;
        interface
    }

    #[test]
    fn retained_configs_match_the_exact_address_and_live_segment() {
        // Full descriptions preserve placement differences for one function,
        // deduplicate repeated descriptions, and expose omitted retained configs.
        let client_ip = "192.0.2.10".parse().unwrap();
        let segment = NetworkSegmentId::from(uuid::Uuid::from_u128(1));
        let other_segment = NetworkSegmentId::from(uuid::Uuid::from_u128(2));
        let config = |function_id, segment, ip| {
            let mut config = InstanceNetworkConfig::for_segment_ids(&[segment], &[], &[]);
            config.interfaces[0].function_id = InterfaceFunctionId::Virtual { id: function_id };
            config.interfaces[0]
                .ip_addrs
                .insert(uuid::Uuid::nil().into(), ip);
            config
        };
        let mut network = config(1, segment, client_ip);
        network
            .interfaces
            .extend(config(4, segment, "192.0.2.11".parse().unwrap()).interfaces);
        network
            .interfaces
            .extend(config(5, other_segment, client_ip).interfaces);
        let mut update = InstanceNetworkConfigUpdate {
            old_config: config(2, segment, client_ip),
            new_config: config(3, segment, client_ip),
        };
        let secondary_locator = DeviceLocator {
            device: "BlueField".into(),
            device_instance: 1,
        };
        let mut secondary = network.interfaces[0].clone();
        secondary.device_locator = Some(secondary_locator.clone());
        update.old_config.interfaces.push(secondary.clone());
        update.new_config.interfaces.push(secondary);

        let attachments = matching_instance_attachments(
            client_ip,
            &HashSet::from([segment]),
            &network,
            Some(&update),
        );
        assert_eq!(
            attachments,
            HashSet::from([
                InstanceAttachment {
                    function_id: &InterfaceFunctionId::Virtual { id: 1 },
                    device_locator: None
                },
                InstanceAttachment {
                    function_id: &InterfaceFunctionId::Virtual { id: 2 },
                    device_locator: None
                },
                InstanceAttachment {
                    function_id: &InterfaceFunctionId::Virtual { id: 3 },
                    device_locator: None
                },
                InstanceAttachment {
                    function_id: &InterfaceFunctionId::Virtual { id: 1 },
                    device_locator: Some(&secondary_locator)
                },
            ]),
        );
    }

    #[test]
    fn retained_attachments_must_agree_on_function_and_placement() {
        let interfaces = [host_interface(0), host_interface(1)];
        let primary_locator = DeviceLocator {
            device: "BlueField".into(),
            device_instance: 0,
        };
        let secondary_locator = DeviceLocator {
            device_instance: 1,
            ..primary_locator.clone()
        };
        let missing_locator = DeviceLocator {
            device_instance: 2,
            ..primary_locator.clone()
        };
        let dpu_ids_by_device = HashMap::from([(
            primary_locator.device.clone(),
            vec![dpu_machine_id(0), dpu_machine_id(1)],
        )]);
        let implicit_pf = InstanceAttachment {
            function_id: &InterfaceFunctionId::Physical {},
            device_locator: None,
        };
        let primary_pf = InstanceAttachment {
            device_locator: Some(&primary_locator),
            ..implicit_pf
        };
        let secondary_pf = InstanceAttachment {
            device_locator: Some(&secondary_locator),
            ..implicit_pf
        };
        let missing_pf = InstanceAttachment {
            device_locator: Some(&missing_locator),
            ..implicit_pf
        };
        let implicit_vf = InstanceAttachment {
            function_id: &InterfaceFunctionId::Virtual { id: 1 },
            ..implicit_pf
        };
        let primary_vf = InstanceAttachment {
            device_locator: Some(&primary_locator),
            ..implicit_vf
        };
        let missing_vf = InstanceAttachment {
            device_locator: Some(&missing_locator),
            ..implicit_vf
        };
        let other_vf = InstanceAttachment {
            function_id: &InterfaceFunctionId::Virtual { id: 2 },
            ..implicit_vf
        };
        let select = |attachments: Vec<InstanceAttachment<'_>>| {
            select_instance_attachment(
                "192.0.2.10".parse().unwrap(),
                &attachments.into_iter().collect(),
                &interfaces,
                &dpu_ids_by_device,
            )
            .map(|interface| interface.map(|interface| interface.id))
            .map_err(|error| {
                assert!(
                    matches!(error, CarbideError::FailedPrecondition(_)),
                    "{error:?}"
                );
            })
        };

        scenarios!(run = select;
            "omitted and explicit primary placement agree" {
                vec![implicit_pf, primary_pf] => Yields(Some(interfaces[0].id)),
            }
            "explicit secondary placement resolves" {
                vec![secondary_pf] => Yields(Some(interfaces[1].id)),
            }
            "primary and secondary placement conflict" {
                vec![implicit_pf, secondary_pf] => Fails,
            }
            "a PF does not hide a conflicting VF" {
                vec![primary_pf, primary_vf] => Fails,
            }
            "distinct VF functions conflict" {
                vec![implicit_vf, other_vf] => Fails,
            }
            "equivalent VF placements preserve compatibility" {
                vec![implicit_vf, primary_vf] => Yields(None),
            }
            "a sole VF does not require physical placement" {
                vec![missing_vf] => Yields(None),
            }
            "owner-only allocation preserves compatibility" {
                vec![] => Yields(None),
            }
            "explicit PF placement must resolve" {
                vec![missing_pf] => Fails,
            }
        );
    }

    #[test]
    fn explicit_placement_needs_one_host_interface_and_known_implicit_placement() {
        let locator = DeviceLocator {
            device: "BlueField".into(),
            device_instance: 1,
        };
        let explicit = InstanceAttachment {
            function_id: &InterfaceFunctionId::Physical {},
            device_locator: Some(&locator),
        };
        let implicit = InstanceAttachment {
            device_locator: None,
            ..explicit
        };
        let dpu_ids_by_device = HashMap::from([(
            locator.device.clone(),
            vec![dpu_machine_id(0), dpu_machine_id(1)],
        )]);
        let secondary = host_interface(1);
        let mut duplicate = secondary.clone();
        duplicate.id = MachineInterfaceId::from(uuid::Uuid::from_u128(2));
        let select = |(attachments, interfaces): (
            Vec<InstanceAttachment<'_>>,
            Vec<MachineInterfaceSnapshot>,
        )| {
            select_instance_attachment(
                "192.0.2.10".parse().unwrap(),
                &attachments.into_iter().collect(),
                &interfaces,
                &dpu_ids_by_device,
            )
            .map(|interface| interface.map(|interface| interface.id))
            .map_err(|error| {
                assert!(
                    matches!(error, CarbideError::FailedPrecondition(_)),
                    "{error:?}"
                );
            })
        };

        scenarios!(run = select;
            "mapped DPU without a host interface fails" {
                (vec![explicit], vec![host_interface(0)]) => Fails,
            }
            "mapped DPU with multiple host interfaces fails" {
                (vec![explicit], vec![secondary.clone(), duplicate]) => Fails,
            }
            "unknown implicit placement cannot agree with an explicit interface" {
                (vec![implicit, explicit], vec![secondary.clone()]) => Fails,
            }
            "omitted placement without primary preserves compatibility" {
                (vec![implicit], vec![secondary]) => Yields(None),
            }
        );
    }
}
