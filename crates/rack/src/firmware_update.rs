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

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, Credentials,
};
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use db::{machine as db_machine, machine_topology as db_machine_topology, switch as db_switch};
use eyre::{Result, eyre};
use librms::protos::rack_manager as rms;
use model::machine::machine_search_config::MachineSearchConfig;
use model::rack::FirmwareUpgradeDeviceInfo;
use model::rack_type::{RackHardwareClass, RackProfile};
use sqlx::PgPool;

use crate::rms_node_type::RmsNodeIdentity;

#[derive(Debug, Clone)]
pub struct RackFirmwareInventory {
    pub machine_ids: Vec<carbide_uuid::machine::HostMachineId>,
    pub machines: Vec<FirmwareUpgradeDeviceInfo>,
    pub switch_ids: Vec<SwitchId>,
    pub switches: Vec<FirmwareUpgradeDeviceInfo>,
}

#[derive(Debug, Clone)]
pub struct RackSwitchFirmwareInventory {
    pub switch_ids: Vec<SwitchId>,
    pub switches: Vec<FirmwareUpgradeDeviceInfo>,
}

pub fn firmware_type_for_profile(profile: &RackProfile) -> &'static str {
    match profile.rack_hardware_class {
        Some(RackHardwareClass::Dev) => "dev",
        Some(RackHardwareClass::Prod) | None => "prod",
    }
}

pub async fn load_rack_firmware_inventory(
    db_pool: &PgPool,
    credential_manager: &dyn CredentialManager,
    rack_id: &RackId,
) -> Result<RackFirmwareInventory> {
    let (machine_ids, machine_topologies, bmc_ips) = {
        let mut txn = db_pool.begin().await?;

        let machine_ids = db_machine::find_machine_ids::<HostMachineId>(
            txn.as_mut(),
            MachineSearchConfig {
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            },
        )
        .await?;

        let machine_topologies =
            db_machine_topology::find_latest_by_machine_ids(txn.as_mut(), &machine_ids).await?;
        // The BMC IP is live network state -- read it from machine_interfaces, not the
        // discovery topology snapshot, so a released or changed lease can't surface a stale IP.
        let bmc_ips: std::collections::HashMap<_, _> =
            db_machine_topology::find_machine_bmc_pairs_by_machine_id(
                txn.as_mut(),
                machine_ids.clone(),
            )
            .await?
            .into_iter()
            .filter_map(|(id, ip)| ip.map(|ip| (id, ip)))
            .collect();

        txn.commit().await?;
        (machine_ids, machine_topologies, bmc_ips)
    };

    let mut machines = Vec::with_capacity(machine_ids.len());
    for machine_id in &machine_ids {
        let topology = machine_topologies
            .get(machine_id)
            .ok_or_else(|| eyre!("machine {} missing topology", machine_id))?;
        let bmc_mac = topology
            .topology()
            .bmc_info
            .mac
            .ok_or_else(|| eyre!("machine {} missing BMC MAC", machine_id))?;
        let bmc_ip = bmc_ips
            .get(machine_id)
            .ok_or_else(|| eyre!("machine {} has no live BMC IP", machine_id))?;
        let (bmc_username, bmc_password) =
            fetch_bmc_credentials(credential_manager, bmc_mac).await?;
        machines.push(FirmwareUpgradeDeviceInfo {
            node_id: machine_id.to_string(),
            mac: bmc_mac.to_string(),
            bmc_ip: bmc_ip.to_string(),
            bmc_username,
            bmc_password,
            os_mac: None,
            os_ip: None,
            os_username: None,
            os_password: None,
            os_hostname: None,
        });
    }

    let RackSwitchFirmwareInventory {
        switch_ids,
        switches,
    } = load_rack_switch_firmware_inventory(db_pool, credential_manager, rack_id).await?;

    Ok(RackFirmwareInventory {
        machine_ids,
        machines,
        switch_ids,
        switches,
    })
}

pub async fn load_rack_switch_firmware_inventory(
    db_pool: &PgPool,
    credential_manager: &dyn CredentialManager,
    rack_id: &RackId,
) -> Result<RackSwitchFirmwareInventory> {
    let (switch_ids, switch_endpoints) = {
        let mut txn = db_pool.begin().await?;

        let switch_ids = db_switch::find_ids(
            txn.as_mut(),
            model::switch::SwitchSearchFilter {
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            },
        )
        .await?;
        let switch_endpoints =
            db_switch::find_switch_endpoints_by_ids(txn.as_mut(), &switch_ids).await?;

        txn.commit().await?;
        (switch_ids, switch_endpoints)
    };

    let mut switches = Vec::with_capacity(switch_endpoints.len());
    for switch in &switch_endpoints {
        switches.push(switch_endpoint_to_firmware_device_info(credential_manager, switch).await?);
    }

    Ok(RackSwitchFirmwareInventory {
        switch_ids,
        switches,
    })
}

pub async fn load_switch_firmware_device_info(
    db_pool: &PgPool,
    credential_manager: &dyn CredentialManager,
    switch_id: &SwitchId,
) -> Result<FirmwareUpgradeDeviceInfo> {
    let switch_endpoint = {
        let mut txn = db_pool.begin().await?;
        let mut switch_endpoints =
            db_switch::find_switch_endpoints_by_ids(txn.as_mut(), std::slice::from_ref(switch_id))
                .await?;
        txn.commit().await?;
        switch_endpoints
            .pop()
            .ok_or_else(|| eyre!("switch {} missing endpoint info", switch_id))?
    };

    switch_endpoint_to_firmware_device_info(credential_manager, &switch_endpoint).await
}

async fn switch_endpoint_to_firmware_device_info(
    credential_manager: &dyn CredentialManager,
    switch: &db_switch::SwitchEndpointRow,
) -> Result<FirmwareUpgradeDeviceInfo> {
    let (bmc_username, bmc_password) =
        fetch_bmc_credentials(credential_manager, switch.bmc_mac).await?;
    let nvos_creds = fetch_nvos_credentials(credential_manager, switch.bmc_mac).await;

    Ok(FirmwareUpgradeDeviceInfo {
        node_id: switch.switch_id.to_string(),
        mac: switch.bmc_mac.to_string(),
        bmc_ip: switch.bmc_ip.to_string(),
        bmc_username,
        bmc_password,
        os_mac: switch.nvos_mac.map(|mac| mac.to_string()),
        os_ip: switch.nvos_ip.map(|ip| ip.to_string()),
        os_username: nvos_creds.as_ref().map(|(username, _)| username.clone()),
        os_password: nvos_creds.map(|(_, password)| password),
        os_hostname: switch
            .nvos_hostname
            .clone()
            .filter(|hostname| !hostname.is_empty()),
    })
}

/// Resolve the per-device BMC root credentials for the given MAC.
///
/// Per-device secrets are authoritative; there is deliberately no site-wide
/// fallback. A missing per-MAC secret means the device has not been
/// (re)ingested, and falling back to the rotating site-wide credential would
/// mask that and break the moment the site rotates.
async fn fetch_bmc_credentials(
    credential_manager: &dyn CredentialManager,
    bmc_mac: mac_address::MacAddress,
) -> Result<(String, String)> {
    let key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: bmc_mac,
        },
    };

    let creds = credential_manager
        .get_credentials(&key)
        .await?
        .ok_or_else(|| {
            eyre!(
                "no per-device BMC credentials found for {bmc_mac}; the device must be (re)ingested"
            )
        })?;

    match creds {
        Credentials::UsernamePassword { username, password } => Ok((username, password)),
    }
}

async fn fetch_nvos_credentials(
    credential_manager: &dyn CredentialManager,
    bmc_mac: mac_address::MacAddress,
) -> Option<(String, String)> {
    let key = CredentialKey::SwitchNvosAdmin {
        bmc_mac_address: bmc_mac,
    };
    match credential_manager.get_credentials(&key).await {
        Ok(Some(Credentials::UsernamePassword { username, password })) => {
            Some((username, password))
        }
        _ => None,
    }
}

pub fn build_new_node_info(
    rack_id: &RackId,
    device: &FirmwareUpgradeDeviceInfo,
    identity: &RmsNodeIdentity,
) -> rms::NodeInfo {
    let bmc_endpoint = if device.bmc_ip.is_empty() || device.mac.is_empty() {
        None
    } else {
        Some(rms::Endpoint {
            interface: Some(rms::NetworkInterface {
                ip_address: device.bmc_ip.clone(),
                mac_address: device.mac.clone(),
                host_name: None,
            }),
            port: 443,
            credentials: user_pass_credentials(&device.bmc_username, &device.bmc_password),
        })
    };

    let host_endpoint = if identity.is_switch() {
        Some(rms::Endpoint {
            interface: build_host_interface(device),
            port: 0,
            credentials: user_pass_credentials(
                device.os_username.as_deref().unwrap_or_default(),
                device.os_password.as_deref().unwrap_or_default(),
            ),
        })
    } else {
        None
    };

    let mut node = rms::NodeInfo {
        node_id: device.node_id.clone(),
        rack_id: rack_id.to_string(),
        r#type: None,
        bmc_endpoint,
        host_endpoint,
        node_descriptor: None,
    };

    identity.apply_to_node_info(&mut node);
    node
}

fn build_host_interface(device: &FirmwareUpgradeDeviceInfo) -> Option<rms::NetworkInterface> {
    let (Some(ip_address), Some(mac_address)) = (&device.os_ip, &device.os_mac) else {
        return None;
    };

    Some(rms::NetworkInterface {
        ip_address: ip_address.clone(),
        mac_address: mac_address.clone(),
        host_name: device
            .os_hostname
            .clone()
            .filter(|hostname| !hostname.is_empty()),
    })
}

fn user_pass_credentials(username: &str, password: &str) -> Option<rms::Credentials> {
    if username.is_empty() || password.is_empty() {
        return None;
    }

    Some(rms::Credentials {
        auth: Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
            username: username.to_string(),
            password: password.to_string(),
        })),
    })
}
