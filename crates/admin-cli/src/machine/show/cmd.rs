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

use std::collections::VecDeque;
use std::fmt::Write;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge as forgerpc;
use carbide_uuid::machine::MachineId;
use carbide_uuid::vpc::VpcId;
use prettytable::{Table, row};
use rpc::Machine;

use super::args::Args;
use crate::cfg::cli_options::SortField;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::{async_write, async_write_table_as_csv, async_writeln};

#[allow(deprecated)]
fn convert_machine_to_nice_format(
    machine: forgerpc::Machine,
    history_count: u32,
) -> CarbideCliResult<String> {
    let mut lines = String::new();
    let sku = machine.hw_sku.unwrap_or_default();
    let sku_device_type = machine.hw_sku_device_type.unwrap_or_default();

    let mut data = vec![
        (
            "ID",
            machine.id.map(|id| id.to_string()).unwrap_or_default(),
        ),
        (
            "RACK_ID",
            machine.rack_id.map(|id| id.to_string()).unwrap_or_default(),
        ),
        ("STATE", machine.state.to_uppercase()),
        ("STATE_VERSION", machine.state_version),
        ("MACHINE TYPE", get_machine_type(machine.id)),
        (
            "FAILURE",
            machine.failure_details.unwrap_or("None".to_string()),
        ),
        ("VERSION", machine.version),
        ("SKU", sku),
        ("SKU DEVICE TYPE", sku_device_type),
        (
            "SLOT NUMBER",
            machine
                .placement_in_rack
                .as_ref()
                .and_then(|p| p.slot_number)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "N/A".to_string()),
        ),
        (
            "TRAY INDEX",
            machine
                .placement_in_rack
                .as_ref()
                .and_then(|p| p.tray_index)
                .map(|t| t.to_string())
                .unwrap_or_else(|| "N/A".to_string()),
        ),
    ];
    if let Some(di) = machine.discovery_info
        && let Some(dmi) = di.dmi_data
    {
        data.push(("VENDOR", dmi.sys_vendor));
        data.push(("PRODUCT NAME", dmi.product_name));
        data.push(("PRODUCT SERIAL", dmi.product_serial));
        data.push(("BOARD SERIAL", dmi.board_serial));
        data.push(("CHASSIS SERIAL", dmi.chassis_serial));
        data.push(("BIOS VERSION", dmi.bios_version));
        data.push(("BOARD VERSION", dmi.board_version));
    }
    let autoupdate = if let Some(autoupdate) = machine.firmware_autoupdate {
        autoupdate.to_string()
    } else {
        "Default".to_string()
    };
    data.push(("FIRMWARE AUTOUPDATE", autoupdate));

    let width = 1 + data
        .iter()
        .fold(0, |accum, (key, _value)| std::cmp::max(accum, key.len()));

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    crate::metadata::write_metadata_in_nice_format(&mut lines, width, machine.metadata.as_ref())?;

    writeln!(&mut lines, "STATE HISTORY: (Latest {history_count} only)")?;
    if machine.events.is_empty() {
        writeln!(&mut lines, "\tEMPTY")?;
    } else {
        let mut max_state_len = 0;
        let mut max_version_len = 0;
        for x in machine
            .events
            .iter()
            .rev()
            .take(history_count as usize)
            .rev()
        {
            max_state_len = max_state_len.max(x.event.len());
            max_version_len = max_version_len.max(x.version.len());
        }
        let header = format!(
            "{:<max_state_len$} {:<max_version_len$} Time",
            "State", "Version"
        );
        writeln!(&mut lines, "\t{header}")?;
        let mut div = "".to_string();
        for _ in 0..header.len() + 27 {
            div.push('-')
        }
        writeln!(&mut lines, "\t{div}")?;
        for x in machine
            .events
            .iter()
            .rev()
            .take(history_count as usize)
            .rev()
        {
            writeln!(
                &mut lines,
                "\t{:<max_state_len$} {:<max_version_len$} {}",
                x.event,
                x.version,
                x.time.unwrap_or_default()
            )?;
        }
    }

    writeln!(&mut lines, "INTERFACES:")?;
    if machine.interfaces.is_empty() {
        writeln!(&mut lines, "\tEMPTY")?;
    } else {
        for (i, interface) in machine.interfaces.into_iter().enumerate() {
            let data = vec![
                ("SN", i.to_string()),
                ("ID", interface.id.unwrap_or_default().to_string()),
                (
                    "DPU ID",
                    interface
                        .attached_dpu_machine_id
                        .as_ref()
                        .map(MachineId::to_string)
                        .unwrap_or_default(),
                ),
                (
                    "Machine ID",
                    interface
                        .machine_id
                        .as_ref()
                        .map(MachineId::to_string)
                        .unwrap_or_default(),
                ),
                (
                    "Segment ID",
                    interface.segment_id.unwrap_or_default().to_string(),
                ),
                (
                    "Domain ID",
                    interface.domain_id.unwrap_or_default().to_string(),
                ),
                ("Hostname", interface.hostname),
                ("Primary", interface.primary_interface.to_string()),
                ("MAC Address", interface.mac_address),
                ("Addresses", interface.address.join(",")),
            ];

            let width = 1 + data
                .iter()
                .fold(0, |accum, (key, _value)| std::cmp::max(accum, key.len()));
            for (key, value) in data {
                writeln!(&mut lines, "\t{key:<width$}: {value}")?;
            }
            writeln!(
                &mut lines,
                "\t--------------------------------------------------"
            )?;
        }
    }

    if let Some(health) = machine.health
        && !health.alerts.is_empty()
    {
        writeln!(&mut lines, "ALERTS:")?;
        for alert in health.alerts {
            writeln!(&mut lines, "\t- {}", alert.message)?;
        }
    }

    Ok(lines)
}

fn get_machine_type(machine_id: Option<MachineId>) -> String {
    machine_id
        .map(|id| id.machine_type().to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

#[allow(deprecated)]
fn convert_machines_to_nice_table(machines: forgerpc::MachineList) -> Box<Table> {
    let mut table = Box::new(Table::new());

    table.set_titles(row![
        "",
        "Id",
        "State",
        "State Version",
        "Attached DPUs",
        "Primary Interface",
        "IP Address",
        "MAC Address",
        "Type",
        "Vendor",
        "Slot",
        "Tray",
        "Labels",
    ]);

    for machine in machines.machines {
        let machine_id_string = machine.id.map(|id| id.to_string()).unwrap_or_default();
        let mut machine_interfaces = machine
            .interfaces
            .into_iter()
            .filter(|x| x.primary_interface)
            .collect::<Vec<forgerpc::MachineInterface>>();

        let (id, address, mac, machine_type, dpu_id) = if machine_interfaces.is_empty() {
            (
                "None".to_string(),
                "None".to_string(),
                "None".to_string(),
                "None".to_string(),
                "None".to_string(),
            )
        } else {
            let mi = machine_interfaces.remove(0);
            let dpu_ids = if !machine.associated_dpu_machine_ids.is_empty() {
                machine
                    .associated_dpu_machine_ids
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
            } else {
                vec![
                    mi.attached_dpu_machine_id
                        .map(|i| i.to_string())
                        .unwrap_or_else(|| "NA".to_string()),
                ]
            };

            (
                mi.id.unwrap_or_default().to_string(),
                mi.address.join(","),
                mi.mac_address,
                get_machine_type(machine.id),
                dpu_ids.join("\n"),
            )
        };
        let mut vendor = String::new();
        if let Some(di) = machine.discovery_info
            && let Some(dmi) = di.dmi_data
        {
            vendor = dmi.sys_vendor;
        }

        let labels = crate::metadata::fmt_labels_as_kv_pairs(machine.metadata.as_ref());

        let slot_number = machine
            .placement_in_rack
            .as_ref()
            .and_then(|p| p.slot_number)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let tray_index = machine
            .placement_in_rack
            .as_ref()
            .and_then(|p| p.tray_index)
            .map(|t| t.to_string())
            .unwrap_or_default();

        let is_unhealthy = machine
            .health
            .map(|x| !x.alerts.is_empty())
            .unwrap_or_default();

        table.add_row(row![
            String::from(if is_unhealthy { "U" } else { "H" }),
            machine_id_string,
            machine.state.to_uppercase(),
            machine.state_version,
            dpu_id,
            id,
            address,
            mac,
            machine_type,
            vendor,
            slot_number,
            tray_index,
            labels.join(", ")
        ]);
    }

    table
}

/// `memory_device_groups` didn't exist before condensing was introduced; rehydrate and clear it
/// on both the deprecated top-level `discovery_info` and `status.discovery_info` so a raw JSON
/// dump of `machine` stays byte-for-byte identical to the pre-condensing output, which only ever
/// had `memory_devices`.
#[allow(deprecated)]
fn rehydrate_machine_memory_devices(machine: &mut rpc::Machine) -> CarbideCliResult<()> {
    if let Some(discovery_info) = machine.discovery_info.as_mut() {
        discovery_info.rehydrate_memory_devices()?;
    }
    if let Some(discovery_info) = machine
        .status
        .as_mut()
        .and_then(|s| s.discovery_info.as_mut())
    {
        discovery_info.rehydrate_memory_devices()?;
    }
    Ok(())
}

#[allow(deprecated)]
async fn show_all_machines(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: &OutputFormat,
    api_client: &ApiClient,
    search_config: rpc::forge::MachineSearchConfig,
    page_size: usize,
    sort_by: &SortField,
) -> CarbideCliResult<()> {
    let mut machines = api_client
        .get_all_machines(search_config, page_size)
        .await?;

    match sort_by {
        SortField::PrimaryId => machines.machines.sort_by_key(|machine| machine.id),
        SortField::State => machines.machines.sort_by(|m1, m2| m1.state.cmp(&m2.state)),
    };

    match output_format {
        OutputFormat::Json => {
            for machine in machines.machines.iter_mut() {
                if let Err(e) = rehydrate_machine_memory_devices(machine) {
                    // we log the error but continue the iteration, so one machine with
                    // malformed memory_device_groups doesn't blank out the whole listing.
                    // rehydrate_memory_devices() leaves memory_device_groups uncleared on
                    // error, so this machine's JSON keeps the grouped shape instead of the
                    // legacy memory_devices shape other machines get. That's intentional:
                    // clearing the groups here would force us to either fabricate a
                    // memory_devices list from data we just rejected, or emit an empty one
                    // that looks like "no memory" — both are misleading. Surfacing the raw,
                    // ungrouped-but-unconverted data is more honest than a plausible-looking
                    // but wrong legacy-shaped record.
                    eprintln!(
                        "Could not rehydrate memory devices for machine {}: {e}",
                        machine.id.map(|id| id.to_string()).unwrap_or_default()
                    );
                }
            }
            async_writeln!(output_file, "{}", serde_json::to_string_pretty(&machines)?)?;
        }
        OutputFormat::AsciiTable => {
            let table = convert_machines_to_nice_table(machines);
            async_write!(output_file, "{}", table)?;
        }
        OutputFormat::Csv => {
            let table = convert_machines_to_nice_table(machines);
            async_write_table_as_csv!(output_file, table)?;
        }
        OutputFormat::Yaml => {
            return Err(CarbideCliError::NotImplemented(output_format.to_string()));
        }
    }
    Ok(())
}

#[allow(deprecated)]
async fn show_machine_information(
    machine_id: MachineId,
    args: &Args,
    output_format: &OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let mut machine = api_client.get_machine(machine_id).await?;
    match output_format {
        OutputFormat::Json => {
            rehydrate_machine_memory_devices(&mut machine)?;
            async_write!(output_file, "{}", serde_json::to_string_pretty(&machine)?)?
        }
        OutputFormat::AsciiTable => async_write!(
            output_file,
            "{}",
            convert_machine_to_nice_format(machine, args.history_count)
                .unwrap_or_else(|x| x.to_string())
        )?,
        OutputFormat::Csv => {
            return Err(CarbideCliError::NotImplemented(
                "CSV formatted output".to_string(),
            ));
        }
        OutputFormat::Yaml => {
            return Err(CarbideCliError::NotImplemented(
                "YAML formatted output".to_string(),
            ));
        }
    }
    Ok(())
}

pub(crate) async fn handle_show(
    args: Args,
    output_format: &OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
    page_size: usize,
    sort_by: &SortField,
) -> CarbideCliResult<()> {
    if let Some(machine_id) = args.machine {
        show_machine_information(machine_id, &args, output_format, output_file, api_client).await?;
    } else {
        // Show both hosts and DPUs if neither flag is specified
        let show_all_types = !args.dpus && !args.hosts;
        let dpus_only = args.dpus && !args.hosts;
        let search_config = rpc::forge::MachineSearchConfig {
            include_dpus: args.dpus || show_all_types,
            exclude_hosts: dpus_only,
            include_predicted_host: args.hosts || show_all_types,
            ..Default::default()
        };
        show_all_machines(
            output_file,
            output_format,
            api_client,
            search_config,
            page_size,
            sort_by,
        )
        .await?;
    }

    Ok(())
}

#[allow(deprecated)]
pub(crate) async fn get_next_free_machine(
    api_client: &ApiClient,
    machine_ids: &mut VecDeque<MachineId>,
    min_interface_count: usize,
    flat_vpc_id: Option<VpcId>,
) -> Option<Machine> {
    get_next_free_machine_inner(
        api_client,
        machine_ids,
        min_interface_count,
        flat_vpc_id,
        None,
    )
    .await
}

/// Same selection logic as [`get_next_free_machine`], but reads from a
/// caller-provided `prefetched` lookup instead of issuing a single-machine
/// `get_machine` RPC per candidate. Callers that already know the full
/// candidate set (e.g. an explicit `--machine-id` list) should batch-resolve
/// it once via a chunked `find_machines_by_ids` call and pass the result
/// here, rather than looping this once per machine -- a few thousand
/// individual lookups is exactly the pattern that trips per-client admission
/// control at fleet scale.
#[allow(deprecated)]
pub(crate) async fn get_next_free_machine_prefetched(
    api_client: &ApiClient,
    machine_ids: &mut VecDeque<MachineId>,
    min_interface_count: usize,
    flat_vpc_id: Option<VpcId>,
    prefetched: &std::collections::HashMap<MachineId, Machine>,
) -> Option<Machine> {
    get_next_free_machine_inner(
        api_client,
        machine_ids,
        min_interface_count,
        flat_vpc_id,
        Some(prefetched),
    )
    .await
}

#[allow(deprecated)]
async fn get_next_free_machine_inner(
    api_client: &ApiClient,
    machine_ids: &mut VecDeque<MachineId>,
    min_interface_count: usize,
    flat_vpc_id: Option<VpcId>,
    prefetched: Option<&std::collections::HashMap<MachineId, Machine>>,
) -> Option<Machine> {
    while let Some(id) = machine_ids.pop_front() {
        tracing::debug!(
            machine_id = %id,
            "Checking machine",
        );
        let looked_up = match prefetched {
            Some(cache) => cache.get(&id).cloned().ok_or(()),
            None => api_client.get_machine(id).await.map_err(|_| ()),
        };
        if let Ok(machine) = looked_up {
            if machine.state != "Ready" {
                tracing::debug!("Machine is not ready");
                continue;
            }
            if flat_vpc_id.is_some() {
                if machine
                    .instance_network_restrictions
                    .as_ref()
                    .is_some_and(|r| {
                        r.network_segment_membership_type()
                            == forgerpc::InstanceNetworkSegmentMembershipType::Static
                    })
                {
                    return Some(machine);
                } else {
                    tracing::debug!(machine_id = %id, "machine does not support flat VPC auto allocation");
                    continue;
                }
            }
            if let Some(discovery_info) = &machine.discovery_info {
                let dpu_interfaces = discovery_info
                    .network_interfaces
                    .iter()
                    .filter(|i| {
                        i.pci_properties.as_ref().is_some_and(|pci_properties| {
                            pci_properties
                                .vendor
                                .to_ascii_lowercase()
                                .contains("mellanox")
                        })
                    })
                    .count();

                if dpu_interfaces >= min_interface_count && machine.state == "Ready" {
                    return Some(machine);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use rpc::DiscoveryInfo;
    use rpc::errors::RpcDataConversionError;
    use rpc::machine_discovery::{MemoryDevice, MemoryDeviceGroup};

    use super::*;

    fn group(size_mb: u32, mem_type: &str, count: u32) -> MemoryDeviceGroup {
        MemoryDeviceGroup {
            size_mb: Some(size_mb),
            mem_type: Some(mem_type.to_string()),
            count,
        }
    }

    #[allow(deprecated)]
    fn machine_with_discovery_info(
        top: Option<DiscoveryInfo>,
        status: Option<DiscoveryInfo>,
    ) -> Machine {
        Machine {
            discovery_info: top,
            status: Some(forgerpc::MachineStatus {
                discovery_info: status,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    #[allow(deprecated)]
    fn rehydrates_grouped_records_in_both_discovery_locations() {
        let mut machine = machine_with_discovery_info(
            Some(DiscoveryInfo {
                memory_device_groups: vec![group(16384, "DDR5", 2)],
                ..Default::default()
            }),
            Some(DiscoveryInfo {
                memory_device_groups: vec![group(8192, "DDR4", 3)],
                ..Default::default()
            }),
        );

        rehydrate_machine_memory_devices(&mut machine).unwrap();

        let top = machine.discovery_info.as_ref().unwrap();
        assert_eq!(
            top.memory_devices,
            vec![
                MemoryDevice {
                    size_mb: Some(16384),
                    mem_type: Some("DDR5".to_string()),
                };
                2
            ]
        );
        assert!(top.memory_device_groups.is_empty());

        let status = machine
            .status
            .as_ref()
            .unwrap()
            .discovery_info
            .as_ref()
            .unwrap();
        assert_eq!(
            status.memory_devices,
            vec![
                MemoryDevice {
                    size_mb: Some(8192),
                    mem_type: Some("DDR4".to_string()),
                };
                3
            ]
        );
        assert!(status.memory_device_groups.is_empty());
    }

    #[test]
    #[allow(deprecated)]
    fn zero_count_groups_leave_legacy_memory_devices_untouched_in_both_locations() {
        let legacy = vec![MemoryDevice {
            size_mb: Some(8192),
            mem_type: Some("DDR4".to_string()),
        }];
        let mut machine = machine_with_discovery_info(
            Some(DiscoveryInfo {
                memory_device_groups: vec![group(16384, "DDR5", 0)],
                memory_devices: legacy.clone(),
                ..Default::default()
            }),
            Some(DiscoveryInfo {
                memory_device_groups: vec![group(16384, "DDR5", 0)],
                memory_devices: legacy.clone(),
                ..Default::default()
            }),
        );

        rehydrate_machine_memory_devices(&mut machine).unwrap();

        let top = machine.discovery_info.as_ref().unwrap();
        assert_eq!(top.memory_devices, legacy);
        assert!(top.memory_device_groups.is_empty());

        let status = machine
            .status
            .as_ref()
            .unwrap()
            .discovery_info
            .as_ref()
            .unwrap();
        assert_eq!(status.memory_devices, legacy);
        assert!(status.memory_device_groups.is_empty());
    }

    #[test]
    #[allow(deprecated)]
    fn aggregate_count_above_max_is_rejected() {
        let max = MemoryDeviceGroup::MAX_REHYDRATE_COUNT;
        let big_group = group(8192, "DDR4", max / 2 + 1);
        let big_discovery_info = || DiscoveryInfo {
            memory_device_groups: vec![big_group.clone(), big_group.clone()],
            ..Default::default()
        };

        for mut machine in [
            machine_with_discovery_info(Some(big_discovery_info()), None),
            machine_with_discovery_info(None, Some(big_discovery_info())),
        ] {
            let err = rehydrate_machine_memory_devices(&mut machine).unwrap_err();
            assert!(matches!(
                err,
                CarbideCliError::RpcDataConversionError(
                    RpcDataConversionError::MemoryDeviceCountExceeded(_, m)
                ) if m == max
            ));
        }
    }

    #[test]
    #[allow(deprecated)]
    fn json_dump_contains_memory_devices_and_omits_memory_device_groups() {
        let mut machine = machine_with_discovery_info(
            Some(DiscoveryInfo {
                memory_device_groups: vec![group(16384, "DDR5", 2)],
                ..Default::default()
            }),
            Some(DiscoveryInfo {
                memory_device_groups: vec![group(8192, "DDR4", 1)],
                ..Default::default()
            }),
        );

        rehydrate_machine_memory_devices(&mut machine).unwrap();

        let json = serde_json::to_string(&machine).unwrap();
        assert!(json.contains("memory_devices"));
        assert!(json.contains("DDR5"));
        assert!(json.contains("DDR4"));
        assert!(!json.contains("memory_device_groups"));
    }
}
