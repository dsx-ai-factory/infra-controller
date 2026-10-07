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

use std::collections::HashSet;
use std::net::IpAddr;

use ::rpc::forge as rpc;
use carbide_uuid::machine::{DpuMachineId, MachineIdSubtypeTrait};
use carbide_uuid::network::NetworkSegmentId;
use db::ObjectColumnFilter;
use db::db_read::DbReader;
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
    Instance(Box<InstanceSnapshot>),
}

impl From<InstanceSnapshot> for ResolvedClient {
    fn from(snapshot: InstanceSnapshot) -> Self {
        ResolvedClient::Instance(Box::new(snapshot))
    }
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
        (None, OverlayAddressOwnerLookup::One(owner)) => Ok(owner.instance.into()),
        (Some(machine_interface), OverlayAddressOwnerLookup::One(owner))
            if is_same_host_inband_interface(&machine_interface, &owner) =>
        {
            Ok(match preferred_lookup {
                PreferredLookup::MachineInterface => machine_interface.into(),
                PreferredLookup::Instance => owner.instance.into(),
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
/// generation. For direct-interface IPs this returns the matching
/// interface; for tenant-allocated IPs it resolves through the instance
/// to the host's machine_interfaces, and prefers an admin-segment one.
pub(super) async fn resolve_machine_interface(
    conn: &mut PgConnection,
    client_ip: IpAddr,
) -> Result<MachineInterfaceSnapshot, CarbideError> {
    match resolve_client_ip(conn, client_ip, PreferredLookup::MachineInterface).await? {
        ResolvedClient::MachineInterface(iface) => Ok(*iface),
        ResolvedClient::Instance(instance) => {
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

            let admin_segment_ids: HashSet<_> = db::network_segment::admin(&mut *conn)
                .await?
                .into_iter()
                .map(|s| s.id)
                .collect();

            host_interfaces
                .iter()
                .find(|i| admin_segment_ids.contains(&i.segment_id))
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
        if let ResolvedClient::Instance(instance) = &resolved
            && let Some(managed_host_state) =
                db::machine::lookup_managed_host_state(&mut *conn, instance.machine_id.into())
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
                    instance_id=%instance.id,
                    machine_id=%instance.machine_id,
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
        ResolvedClient::Instance(instance) => {
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
            let domain_id = machine_interface.domain_id.ok_or_else(|| {
                CarbideError::internal(format!(
                    "machine interface did not have an associated domain {}",
                    machine_interface.id
                ))
            })?;

            let domain = db::dns::domain::find_by_uuid(&mut *conn, domain_id)
                .await
                .map_err(CarbideError::from)?
                .ok_or_else(|| {
                    CarbideError::internal(format!("could not find domain with id {domain_id}"))
                })?
                .to_owned();

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
                    domain: Some(rpc::PxeDomain {
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
