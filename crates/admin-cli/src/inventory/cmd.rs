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
use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::fs;

use ::rpc::site_explorer::ExploredManagedHost;
use ::rpc::{InstanceList, MachineList};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use serde::{Deserialize, Serialize};

use super::args::Cmd;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

// Expected output
// x86_host_bmcs:
//   - all hosts BMC
//
// x86_hosts:
//   - all hosts on admin network, not on tenant network
//
// dpus:
//   - all dpus
//
// instances:
//   children:
//     - tenant_org1
//     - tenant_org2
//
// tenant_org1:
//   - all instances in tenant_org1
//
// Each host/dpu/tenant:
//   ansible_host: IP Address
//   BMC_IP: IP Address
//
type InstanceGroup<'a> = HashMap<&'static str, HashMap<&'a str, Option<&'a str>, RandomState>>;

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum TopYamlElement<'a> {
    InstanceChildren(InstanceGroup<'a>),
    Instance(HashMap<String, HashMap<InstanceId, InstanceDetails<'a>>>),
    BmcHostInfo(HashMap<String, HashMap<String, BmcInfo<'a>>>),
    HostMachineInfo(HashMap<String, HashMap<&'a str, HostMachineInfo<'a>>>),
    DpuMachineInfo(HashMap<String, HashMap<&'a str, DpuMachineInfo<'a>>>),
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct BmcInfo<'a> {
    ansible_host: &'a str,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_bmc_ip: Option<&'a str>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct HostMachineInfo<'a> {
    ansible_host: &'a str,
    machine_id: Option<MachineId>,
    // Deprecated field. Use all_dpu_machine_ids or primary_dpu_machine_id for primary dpu.
    dpu_machine_id: Option<MachineId>,
    // Primary DPU
    primary_dpu_machine_id: Option<MachineId>,
    all_dpu_machine_ids: Vec<MachineId>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct DpuMachineInfo<'a> {
    ansible_host: &'a str,
    machine_id: Option<MachineId>,
}

/// Generate element containing all information needed to write a Machine Host.
#[allow(deprecated)]
fn get_host_machine_info<'a>(
    machines: &'a [&'a ::rpc::Machine],
) -> HashMap<&'a str, HostMachineInfo<'a>> {
    let mut machine_element: HashMap<&'a str, HostMachineInfo> = HashMap::new();

    for machine in machines {
        let primary_interface = machine.interfaces.iter().find(|x| x.primary_interface);

        if let Some(primary_interface) = primary_interface {
            let hostname = primary_interface.hostname.as_str();
            let address = primary_interface.address[0].as_str();
            let primary_dpu = primary_interface.attached_dpu_machine_id;

            machine_element.insert(
                hostname,
                HostMachineInfo {
                    ansible_host: address,
                    machine_id: machine.id,
                    dpu_machine_id: primary_dpu.map(Into::into),
                    primary_dpu_machine_id: primary_dpu.map(Into::into),
                    all_dpu_machine_ids: machine
                        .interfaces
                        .iter()
                        .filter_map(|x| x.attached_dpu_machine_id.map(Into::into))
                        .collect(),
                },
            );
        } else {
            eprintln!(
                "Ignoring machine {:?} since no attached primary interface found with it.",
                machine.id
            )
        }
    }

    machine_element
}

/// Generate element containing all information needed to write a Machine Host.
#[allow(deprecated)]
fn get_dpu_machine_info<'a>(
    machines: &'a [&'a ::rpc::Machine],
) -> HashMap<&'a str, DpuMachineInfo<'a>> {
    let mut machine_element: HashMap<&'a str, DpuMachineInfo> = HashMap::new();

    for machine in machines {
        let primary_interface = machine.interfaces.iter().find(|x| x.primary_interface);

        if let Some(primary_interface) = primary_interface {
            let hostname = primary_interface.hostname.as_str();
            let address = primary_interface.address[0].as_str();

            machine_element.insert(
                hostname,
                DpuMachineInfo {
                    ansible_host: address,
                    machine_id: machine.id,
                },
            );
        }
    }

    machine_element
}

/// Generate element containing all information needed to write a BMC Host.
#[allow(deprecated)]
fn get_bmc_info<'a>(
    machines: &[&'a ::rpc::Machine],
    managed_hosts: &'a [ExploredManagedHost],
) -> HashMap<String, BmcInfo<'a>> {
    let mut bmc_element: HashMap<String, BmcInfo<'a>> = HashMap::new();
    let mut known_ips: Vec<&'a str> = Vec::new();
    let mut managed_host_map: HashMap<&'a str, &'a str> = HashMap::new();

    for managed_host in managed_hosts {
        for dpu in &managed_host.dpus {
            managed_host_map.insert(dpu.bmc_ip.as_str(), managed_host.host_bmc_ip.as_str());
        }
    }

    for machine in machines {
        let Some(bmc_ip) = machine.bmc_info.as_ref().and_then(|x| x.ip.as_deref()) else {
            continue;
        };

        let hostname = machine
            .interfaces
            .iter()
            .find_map(|x| {
                if x.primary_interface {
                    Some(x.hostname.as_str())
                } else {
                    None
                }
            })
            .unwrap_or("Not Found");

        bmc_element.insert(
            format!("{hostname}-bmc"),
            BmcInfo {
                ansible_host: bmc_ip,
                host_bmc_ip: managed_host_map.get(&bmc_ip).copied(),
            },
        );

        known_ips.push(bmc_ip);
    }

    for managed_host in managed_hosts {
        for dpu in &managed_host.dpus {
            if !known_ips.contains(&dpu.bmc_ip.as_str()) {
                // Found a undiscovered dpu bmc ip.
                bmc_element.insert(
                    format!("{}-undiscovered-bmc", dpu.bmc_ip),
                    BmcInfo {
                        ansible_host: dpu.bmc_ip.as_str(),
                        host_bmc_ip: Some(managed_host.host_bmc_ip.as_str()),
                    },
                );
            }
        }
    }

    bmc_element
}

/// Main entry function which print inventory.
pub(super) async fn print_inventory(
    api_client: &ApiClient,
    action: Cmd,
    page_size: usize,
) -> CarbideCliResult<()> {
    let all_machines = api_client
        .get_all_machines(
            rpc::forge::MachineSearchConfig {
                include_predicted_host: true,
                include_dpus: true,
                ..Default::default()
            },
            page_size,
        )
        .await?;
    let all_instances = api_client
        .get_all_instances(None, None, None, None, None, page_size)
        .await?;

    let (instances, used_machine) = create_inventory_for_instances(&all_instances, &all_machines)?;

    let children: InstanceGroup = HashMap::from([(
        "children",
        HashMap::from_iter(instances.keys().map(|x| (*x, None))),
    )]);

    let mut final_group: HashMap<String, TopYamlElement> = HashMap::from([(
        "instances".to_string(),
        TopYamlElement::InstanceChildren(children),
    )]);

    let site_report_managed_host = api_client.get_all_explored_managed_hosts(page_size).await?;

    for (key, value) in instances.into_iter() {
        let mut ins_details: HashMap<InstanceId, InstanceDetails> = HashMap::new();

        for ins in value {
            if let Some(instance_id) = ins.instance_id {
                ins_details.insert(instance_id, ins);
            }
        }
        final_group.insert(
            key.to_string(),
            TopYamlElement::Instance(HashMap::from([("hosts".to_string(), ins_details)])),
        );
    }

    let all_hosts = all_machines
        .machines
        .iter()
        .filter(|m| m.id.is_some_and(|id| id.machine_type().is_host()))
        .collect::<Vec<&::rpc::Machine>>();

    let all_dpus = all_machines
        .machines
        .iter()
        .filter(|m| m.id.is_some_and(|id| id.machine_type().is_dpu()))
        .collect::<Vec<&::rpc::Machine>>();

    final_group.insert(
        "x86_host_bmcs".to_string(),
        TopYamlElement::BmcHostInfo(HashMap::from([(
            "hosts".to_string(),
            get_bmc_info(&all_hosts, &[]),
        )])),
    );
    final_group.insert(
        "dpu_bmcs".to_string(),
        TopYamlElement::BmcHostInfo(HashMap::from([(
            "hosts".to_string(),
            get_bmc_info(&all_dpus, &site_report_managed_host),
        )])),
    );
    let host_on_admin = all_hosts
        .into_iter()
        .filter(|x| !used_machine.contains(&x.id))
        .collect::<Vec<&::rpc::Machine>>();

    final_group.insert(
        "x86_hosts".to_string(),
        TopYamlElement::HostMachineInfo(HashMap::from([(
            "hosts".to_string(),
            get_host_machine_info(&host_on_admin),
        )])),
    );
    final_group.insert(
        "dpus".to_string(),
        TopYamlElement::DpuMachineInfo(HashMap::from([(
            "hosts".to_string(),
            get_dpu_machine_info(&all_dpus),
        )])),
    );
    let output = serde_yaml::to_string(&final_group).map_err(CarbideCliError::YamlError)?;
    if let Some(filename) = action.filename {
        fs::write(filename, output)
            .map_err(|e| CarbideCliError::GenericError(format!("File write error: {e}")))?;
    } else {
        println!("{output}");
    }
    Ok(())
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct InstanceDetails<'a> {
    instance_id: Option<InstanceId>,
    machine_id: Option<MachineId>,
    ansible_host: &'a str,
    bmc_ip: &'a str,
}

type CreateInventoryReturnType<'a> = (
    HashMap<&'a str, Vec<InstanceDetails<'a>>>,
    Vec<Option<MachineId>>,
);

/// Generate inventory item for instances.
#[allow(deprecated)]
fn create_inventory_for_instances<'a>(
    instances: &'a InstanceList,
    machines: &'a MachineList,
) -> CarbideCliResult<CreateInventoryReturnType<'a>> {
    let mut tenant_map: HashMap<&'a str, Vec<InstanceDetails>> = HashMap::new();
    let mut used_machines = vec![];

    for instance in &instances.instances {
        let if_status = instance
            .status
            .as_ref()
            .and_then(|status| status.network.as_ref())
            .map(|status| status.interfaces.as_slice())
            .unwrap_or_default();

        let physical_ip = if_status.iter().find_map(|x| {
            // For physical interface `virtual_function_id` is None.
            if x.virtual_function_id.is_none() {
                x.addresses.first().map(String::as_str)
            } else {
                None
            }
        });

        let machine = machines
            .machines
            .iter()
            .find(|x| x.id == instance.machine_id.map(Into::into))
            .ok_or_else(|| {
                CarbideCliError::GenericError(format!(
                    "No such machine {:?} found in db, instance {:?}",
                    instance.machine_id, instance.id,
                ))
            })?;

        used_machines.push(machine.id);

        let bmc_ip = machine
            .bmc_info
            .as_ref()
            .and_then(|x| x.ip.as_deref())
            .unwrap_or_default();

        let details = InstanceDetails {
            instance_id: instance.id,
            machine_id: instance.machine_id.map(Into::into),
            ansible_host: physical_ip.unwrap_or_default(),
            bmc_ip,
        };

        let tenant = instance
            .config
            .as_ref()
            .and_then(|x| x.tenant.as_ref())
            .map(|x| x.tenant_organization_id.as_str())
            .unwrap_or("Unknown");

        tenant_map.entry(tenant).or_default().push(details);
    }

    Ok((tenant_map, used_machines))
}
