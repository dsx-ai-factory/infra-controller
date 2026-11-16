// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_uuid::device::DeviceId;
use component_manager::component_common::{BmcEndpoint, BmcVendor};

use super::{
    Api, ComponentManager, ComponentManagerError, HashMap, IpAddr, MacAddress, MachineSearchConfig,
    NoneIfEmpty, RackComponent, Status, SwitchEndpoint, component_manager_error_to_result,
    component_manager_error_to_status, db, invalid_mac_result, mac_result, mac_status_result,
    resolve_compute_macs, resolve_credentials, resolve_power_shelf_macs, resolve_switch_macs, rpc,
};

// Switch resets carry both BMC and NVOS endpoints for every action.
// Compute and power-shelf resets carry only their management-controller endpoint.
pub(super) struct ResetEndpoint {
    pub(super) bmc: BmcEndpoint,
    switch: Option<SwitchEndpoint>,
}

pub(super) fn untracked_status(
    target: &rpc::get_component_reset_status_request::Target,
) -> rpc::GetComponentResetStatusResponse {
    use rpc::get_component_reset_status_request::Target;
    // No job is tracked: echo the targets without querying inventory or the BMC.
    let identities: Vec<String> = match target {
        Target::MachineIds(list) => list.machine_ids.iter().map(ToString::to_string).collect(),
        Target::SwitchIds(list) => list.ids.iter().map(ToString::to_string).collect(),
        Target::PowerShelfIds(list) => list.ids.iter().map(ToString::to_string).collect(),
        Target::ComputeBmcMacs(list)
        | Target::SwitchBmcMacs(list)
        | Target::PowerShelfPmcMacs(list) => list.mac_addresses.clone(),
    };
    let results = identities.into_iter().map(|identity| match target {
        Target::ComputeBmcMacs(_) | Target::SwitchBmcMacs(_) | Target::PowerShelfPmcMacs(_) => {
            match identity.parse::<MacAddress>() {
                Ok(mac) => mac_result(&mac, rpc::ComponentManagerStatusCode::Success, None),
                Err(_) => invalid_mac_result(&identity),
            }
        }
        _ => rpc::ComponentResult {
            component_id: Some(identity),
            status: rpc::ComponentManagerStatusCode::Success as i32,
            ..Default::default()
        },
    });
    rpc::GetComponentResetStatusResponse {
        statuses: results
            .map(|result| rpc::ComponentResetStatus {
                state: if result.status == rpc::ComponentManagerStatusCode::Success as i32 {
                    rpc::ComponentResetState::Completed as i32
                } else {
                    rpc::ComponentResetState::Unknown as i32
                },
                result: Some(result),
                action: rpc::ComponentResetAction::BmcReset as i32,
            })
            .collect(),
    }
}

pub(super) async fn endpoint_by_id(api: &Api, id: DeviceId) -> Result<ResetEndpoint, Status> {
    let (ip, mac, component, nvos) = match id {
        DeviceId::Machine(id) => {
            let machine =
                db::machine::find_one(&mut api.db_reader(), &id, MachineSearchConfig::default())
                    .await?
                    .ok_or_else(|| Status::not_found("machine not found"))?;
            (
                machine
                    .status
                    .bmc_info
                    .ip
                    .ok_or_else(|| Status::not_found("machine BMC IP not found"))?,
                machine
                    .status
                    .bmc_info
                    .mac
                    .ok_or_else(|| Status::not_found("machine BMC MAC not found"))?,
                RackComponent::ComputeBmc,
                None,
            )
        }
        DeviceId::Switch(id) => {
            let row = db::switch::find_switch_endpoints_by_ids(&mut api.db_reader(), &[id])
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| Status::not_found("switch BMC endpoint not found"))?;
            (
                row.bmc_ip,
                row.bmc_mac,
                RackComponent::SwitchBmc,
                row.nvos_ip
                    .zip(row.nvos_mac)
                    .map(|(ip, mac)| (ip, mac, row.nvos_hostname)),
            )
        }
        DeviceId::PowerShelf(id) => {
            let row =
                db::power_shelf::find_power_shelf_endpoints_by_ids(&mut api.db_reader(), &[id])
                    .await?
                    .into_iter()
                    .next()
                    .ok_or_else(|| Status::not_found("power shelf PMC endpoint not found"))?;
            (row.pmc_ip, row.pmc_mac, RackComponent::PowerShelfPmc, None)
        }
    };
    prepare_endpoint(api, component, ip, mac, nvos).await
}

pub(super) async fn endpoint_by_mac(
    api: &Api,
    component: RackComponent,
    raw: &String,
) -> Result<(ResetEndpoint, Option<DeviceId>), rpc::ComponentResult> {
    let mac = raw
        .parse::<MacAddress>()
        .map_err(|_| invalid_mac_result(raw))?;
    let id = match component {
        RackComponent::ComputeBmc => resolve_compute_macs(api, std::slice::from_ref(raw))
            .await
            .map(|resolution| {
                resolution
                    .ingested
                    .keys()
                    .next()
                    .map(|id| DeviceId::Machine((*id).into()))
            }),
        RackComponent::SwitchBmc | RackComponent::SwitchNvos => {
            resolve_switch_macs(api, std::slice::from_ref(raw))
                .await
                .map(|resolution| {
                    resolution
                        .ingested
                        .keys()
                        .next()
                        .copied()
                        .map(DeviceId::Switch)
                })
        }
        RackComponent::PowerShelfPmc => resolve_power_shelf_macs(api, std::slice::from_ref(raw))
            .await
            .map(|resolution| {
                resolution
                    .ingested
                    .keys()
                    .next()
                    .copied()
                    .map(DeviceId::PowerShelf)
            }),
    }
    .map_err(|error| mac_status_result(&mac, &error))?;
    let endpoint = match id {
        Some(id) => endpoint_by_id(api, id).await,
        None => endpoint_before_ingestion(api, component, mac).await,
    };
    endpoint.map(|endpoint| (endpoint, id)).map_err(|error| {
        let mut result = mac_status_result(&mac, &error);
        result.component_id = id.map(|id| id.to_string());
        result
    })
}

async fn endpoint_before_ingestion(
    api: &Api,
    component: RackComponent,
    mac: MacAddress,
) -> Result<ResetEndpoint, Status> {
    if matches!(
        component,
        RackComponent::SwitchBmc | RackComponent::SwitchNvos
    ) && let Some(row) =
        db::switch::find_switch_endpoints_by_bmc_macs(&mut api.db_reader(), &[mac])
            .await?
            .into_iter()
            .next()
    {
        return prepare_endpoint(
            api,
            component,
            row.bmc_ip,
            mac,
            row.nvos_ip
                .zip(row.nvos_mac)
                .map(|(ip, mac)| (ip, mac, row.nvos_hostname)),
        )
        .await;
    }
    let ip = db::machine_interface::lookup_bmc_ip_by_mac_address(&mut api.db_reader(), mac)
        .await?
        .into_iter()
        .min()
        .ok_or_else(|| Status::not_found("no BMC interface found for MAC"))?;
    prepare_endpoint(api, component, ip, mac, None).await
}

async fn prepare_endpoint(
    api: &Api,
    component: RackComponent,
    ip: IpAddr,
    mac: MacAddress,
    nvos: Option<(IpAddr, MacAddress, Option<String>)>,
) -> Result<ResetEndpoint, Status> {
    // Reject a known stale IP/MAC pair before obtaining credentials or connecting.
    if let Some(owner) = db::machine_interface::find_by_ip(&mut api.db_reader(), ip).await?
        && owner.mac_address != mac
    {
        return Err(Status::invalid_argument(
            "BMC IP belongs to a different MAC",
        ));
    }
    let vendor = db::explored_endpoints::lookup_bmc_metadata_by_ip(ip, &mut api.db_reader())
        .await?
        .vendor
        .map(|vendor| BmcVendor::from(bmc_vendor::BMCVendor::from(vendor.as_str())))
        .unwrap_or(BmcVendor::Unknown);
    let bmc = BmcEndpoint {
        bmc_ip: ip,
        bmc_mac: mac,
        vendor,
        bmc_credentials: resolve_credentials(api, component, mac, None)
            .await
            .map_err(component_manager_error_to_status)?,
    };
    let switch = if matches!(
        component,
        RackComponent::SwitchBmc | RackComponent::SwitchNvos
    ) {
        let (nvos_ip, nvos_mac, hostname) =
            nvos.ok_or_else(|| Status::not_found("NVOS MAC or IP not available"))?;
        Some(SwitchEndpoint {
            bmc_ip: ip,
            bmc_mac: mac,
            bmc_credentials: bmc.bmc_credentials.clone(),
            nvos_ip,
            nvos_mac,
            nvos_credentials: resolve_credentials(api, RackComponent::SwitchNvos, mac, None)
                .await
                .map_err(component_manager_error_to_status)?,
            nvos_host_name: hostname.none_if_empty(),
        })
    } else {
        None
    };
    Ok(ResetEndpoint { bmc, switch })
}

pub(super) async fn dispatch(
    cm: &ComponentManager,
    component: RackComponent,
    action: rpc::ComponentResetAction,
    domain_name: Option<&str>,
    endpoints: &[ResetEndpoint],
) -> Result<HashMap<MacAddress, rpc::ComponentResult>, ComponentManagerError> {
    let switch_endpoints: Vec<_> = endpoints
        .iter()
        .filter_map(|endpoint| endpoint.switch.clone())
        .collect();
    match action {
        rpc::ComponentResetAction::BmcReset => {
            let bmc_endpoints: Vec<_> = endpoints
                .iter()
                .map(|endpoint| endpoint.bmc.clone())
                .collect();
            let responses = match component {
                RackComponent::ComputeBmc => cm.compute_tray.reset_bmc(&bmc_endpoints).await,
                RackComponent::SwitchBmc | RackComponent::SwitchNvos => {
                    cm.nv_switch.reset_bmc(&switch_endpoints).await
                }
                RackComponent::PowerShelfPmc => cm.power_shelf.reset_bmc(&bmc_endpoints).await,
            }?;
            Ok(responses
                .into_iter()
                .map(|response| {
                    (
                        response.bmc_mac,
                        mac_result(
                            &response.bmc_mac,
                            if response.success {
                                rpc::ComponentManagerStatusCode::Success
                            } else {
                                rpc::ComponentManagerStatusCode::InternalError
                            },
                            response.error,
                        ),
                    )
                })
                .collect())
        }
        rpc::ComponentResetAction::SdnFactoryReset
        | rpc::ComponentResetAction::NvosFactoryReset => {
            if !matches!(
                component,
                RackComponent::SwitchBmc | RackComponent::SwitchNvos
            ) {
                return Err(ComponentManagerError::InvalidArgument(
                    "this reset action requires switch targets".into(),
                ));
            }
            let responses = match action {
                rpc::ComponentResetAction::SdnFactoryReset => {
                    cm.nv_switch
                        .batch_reset_sdn_factory_default(&switch_endpoints, domain_name)
                        .await
                }
                _ => {
                    cm.nv_switch
                        .batch_reset_switch_factory_default(&switch_endpoints, domain_name)
                        .await
                }
            }?;
            Ok(responses
                .into_iter()
                .map(|response| {
                    let outcome = response.result;
                    let mut result = mac_result(
                        &outcome.bmc_mac,
                        if outcome.success {
                            rpc::ComponentManagerStatusCode::Success
                        } else {
                            rpc::ComponentManagerStatusCode::InternalError
                        },
                        None,
                    );
                    if let Some(error) = outcome.error {
                        result = component_manager_error_to_result(result, error);
                    }
                    (outcome.bmc_mac, result)
                })
                .collect())
        }
        rpc::ComponentResetAction::BmcFactoryReset => Err(ComponentManagerError::Unsupported(
            "BMC factory reset is not implemented".into(),
        )),
        rpc::ComponentResetAction::Unspecified => Err(ComponentManagerError::InvalidArgument(
            "a recognized reset action is required".into(),
        )),
    }
}
