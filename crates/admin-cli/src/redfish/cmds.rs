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
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use eyre::eyre;
use libredfish::model::oem::nvidia_dpu::NicMode;
use libredfish::model::software_inventory::SoftwareInventory;
use libredfish::model::task::{Task, TaskState};
use libredfish::model::update_service::ComponentType;
use libredfish::model::{LinkStatus, ResourceStatus};
use libredfish::{
    Boot, Chassis, EnabledDisabled, EthernetInterface, NetworkDeviceFunction, NetworkPort, Redfish,
    RedfishError, RoleId, SystemPowerControl,
};
use mac_address::MacAddress;
use prettytable::{Table, row};
use tracing::warn;

use super::args::{Cmd, DpuOperations, FwCommand, RedfishAction, ShowFw, ShowPort};

pub(crate) async fn action(action: RedfishAction) -> color_eyre::Result<()> {
    let endpoint = libredfish::Endpoint {
        host: action.address,
        user: action.username,
        password: action.password,
        ..Default::default()
    };

    let proxy = std::env::var("http_proxy")
        .ok()
        .or_else(|| std::env::var("https_proxy").ok())
        .or_else(|| std::env::var("HTTP_PROXY").ok())
        .or_else(|| std::env::var("HTTPS_PROXY").ok());

    use Cmd::*;
    let pool = libredfish::RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .proxy(proxy)
        .build()?;
    let redfish: Box<dyn Redfish> = match &action.command {
        ChangeBmcPassword(_) => pool.create_standard_client(endpoint)?,
        _ => pool.create_client(endpoint).await?,
    };
    match action.command {
        BiosAttrs => {
            let bios = redfish.bios().await?;
            println!("{}", serde_json::to_string(&bios)?);
        }
        BootHdd => {
            redfish.boot_first(Boot::HardDisk).await?;
        }
        BootPxe => {
            redfish.boot_first(Boot::Pxe).await?;
        }
        BootUefiHttp => {
            redfish.boot_first(Boot::UefiHttp).await?;
        }
        BootOnceHdd => {
            redfish.boot_once(Boot::HardDisk).await?;
        }
        BootOncePxe => {
            redfish.boot_once(Boot::Pxe).await?;
        }
        BootOnceUefiHttp => {
            redfish.boot_once(Boot::UefiHttp).await?;
        }
        ClearPending => {
            redfish.clear_pending().await?;
        }
        MachineSetup(machine_setup_args) => {
            let bios_profiles = if let Some(profiles_string) = machine_setup_args.bios_profiles {
                let parsed: libredfish::BiosProfileVendor =
                    serde_json::from_str(profiles_string.as_str())?;
                parsed
            } else {
                HashMap::default()
            };
            let selected_profile = machine_setup_args
                .selected_profile
                .unwrap_or(libredfish::BiosProfileType::Performance);

            let boot_interface = machine_setup_args
                .boot_interface_mac
                .as_deref()
                .map(|m| m.parse::<MacAddress>())
                .transpose()
                .map_err(|e| eyre!("invalid boot_interface_mac: {e}"))?
                .map(libredfish::BootInterfaceRef::Mac);
            redfish
                .machine_setup(
                    boot_interface,
                    &bios_profiles,
                    selected_profile,
                    &HashMap::default(),
                )
                .await?;
        }
        MachineSetupStatus(machine_setup_status_args) => {
            let boot_interface = machine_setup_status_args
                .boot_interface_mac
                .as_deref()
                .map(|m| m.parse::<MacAddress>())
                .transpose()
                .map_err(|e| eyre!("invalid boot_interface_mac: {e}"))?
                .map(libredfish::BootInterfaceRef::Mac);
            println!("{}", redfish.machine_setup_status(boot_interface).await?);
        }
        SetForgePasswordPolicy => {
            redfish.set_machine_password_policy().await?;
        }
        GetPowerState => {
            println!("{}", redfish.get_power_state().await?);
        }
        GetBootOption(selector) => {
            if let Some(boot_id) = selector.id {
                println!("{:?}", redfish.get_boot_option(&boot_id).await?)
            } else {
                let all = redfish.get_boot_options().await?;
                for b in all.members {
                    if let Some(id) = b.odata_id.split('/').next_back() {
                        println!("{:?}", redfish.get_boot_option(id).await?)
                    }
                }
            }
        }
        LockdownDisable => {
            redfish.lockdown(EnabledDisabled::Disabled).await?;
            println!("BIOS settings changes require system restart");
        }
        LockdownEnable => {
            redfish.lockdown(EnabledDisabled::Enabled).await?;
            println!("BIOS settings changes require system restart");
        }
        LockdownStatus => {
            println!("{}", redfish.lockdown_status().await?);
        }
        ForceOff => {
            redfish.power(SystemPowerControl::ForceOff).await?;
        }
        On => {
            redfish.power(SystemPowerControl::On).await?;
        }
        PcieDevices => {
            let mut table = Table::new();
            table.set_titles(row![
                "ID",
                "Manufacturer",
                "Name",
                "Firmware version",
                "Part",
                "Serial",
                "Status",
            ]);
            for dev in redfish.pcie_devices().await? {
                let status = dev
                    .status
                    .map(|status| {
                        format!(
                            "{} {}",
                            status.health.unwrap_or_default(),
                            status.state.unwrap_or("".to_string())
                        )
                    })
                    .unwrap_or_default();
                table.add_row(row![
                    dev.id.unwrap_or_default(),
                    dev.manufacturer.or(dev.gpu_vendor).unwrap_or_default(),
                    dev.name.unwrap_or_default(),
                    dev.firmware_version.unwrap_or_default(),
                    dev.part_number.unwrap_or_default(),
                    dev.serial_number.unwrap_or_default(),
                    status,
                ]);
            }
            table.set_format(*prettytable::format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
            table.printstd();
        }
        LocalStorage => {
            let mut table = Table::new();
            table.set_titles(row![
                "ID",
                "Manufacturer",
                "Name",
                "model",
                "capacity(GiB)",
                "revision",
                "Serial",
                "PredictFail",
                "PredictLife",
                "Status",
            ]);
            for dev in redfish.get_drives_metrics().await? {
                let status = dev.status.unwrap_or(ResourceStatus {
                    health: Some(libredfish::model::ResourceHealth::Ok),
                    health_rollup: Some(libredfish::model::ResourceHealth::Ok),
                    state: Some(libredfish::model::ResourceState::Unknown),
                });
                let mut predictlife = 100;
                if let Some(ref _pred_fail) = dev.failure_predicted
                    && *_pred_fail
                {
                    predictlife = 1;
                }
                if dev.predicted_media_life_left_percent.is_none() {
                    // supermicro has percentage_drive_life_used in oem...
                    if let Some(oem) = dev.oem
                        && let Some(supermicro) = oem.supermicro
                    {
                        predictlife =
                            100 - (supermicro.percentage_drive_life_used.unwrap_or(0.0) as i32);
                    }
                }
                table.add_row(row![
                    dev.id.unwrap_or("".to_string()),
                    dev.manufacturer.unwrap_or("".to_string()),
                    dev.name.unwrap_or("".to_string()),
                    dev.model.unwrap_or("".to_string()),
                    dev.capacity_bytes.unwrap_or(0) / 1024 / 1024 / 1024,
                    dev.revision.unwrap_or("".to_string()),
                    dev.serial_number.unwrap_or("".to_string()),
                    dev.failure_predicted.unwrap_or(true),
                    predictlife,
                    format!(
                        "{} {}",
                        status.health.unwrap_or_default(),
                        status
                            .state
                            .unwrap_or(libredfish::model::ResourceState::Unknown)
                    ),
                ]);
            }
            table.set_format(*prettytable::format::consts::FORMAT_NO_LINESEP_WITH_TITLE);
            table.printstd();
        }
        Pending => {
            let pending = redfish.pending().await?;
            println!("{pending:#?}");
        }
        PowerMetrics => {
            let power = redfish.get_power_metrics().await?;
            println!("{}", serde_json::to_string_pretty(&power)?);
        }
        ForceRestart => {
            redfish.power(SystemPowerControl::ForceRestart).await?;
        }
        GracefulRestart => {
            redfish.power(SystemPowerControl::GracefulRestart).await?;
        }
        SerialEnable => {
            redfish.setup_serial_console().await?;
            println!("BIOS settings changes require system restart");
        }
        SerialStatus => {
            println!("{}", redfish.serial_console_status().await?);
        }
        GracefulShutdown => {
            redfish.power(SystemPowerControl::GracefulShutdown).await?;
        }
        ACPowerCycle => {
            redfish.power(SystemPowerControl::ACPowercycle).await?;
        }
        ThermalMetrics => {
            let thermal = redfish.get_thermal_metrics().await?;
            println!("{}", serde_json::to_string_pretty(&thermal)?);
        }
        TpmReset => {
            redfish.clear_tpm().await?;
            println!("BIOS settings changes require system restart");
        }
        BmcResetToDefaults => {
            redfish.bmc_reset_to_defaults().await?;
        }
        BmcReset { reset_type } => {
            redfish
                .bmc_reset(reset_type.map(libredfish::ManagerResetType::from))
                .await?;
        }
        DisableSecureBoot => {
            redfish.disable_secure_boot().await?;
            println!("BIOS settings changes require system restart");
        }
        GetSecureBoot => {
            println!("{:#?}", redfish.get_secure_boot().await?);
        }
        GetBmcAccounts => {
            for u in redfish.get_accounts().await? {
                println!("{u:?}");
            }
        }
        CreateBmcUser(bmc_user) => {
            // clap has already validated --role-id against the allowed set;
            // default to Administrator when it is omitted.
            let role: RoleId = bmc_user
                .role_id
                .map(RoleId::from)
                .unwrap_or(RoleId::Administrator);
            redfish
                .create_user(&bmc_user.user, &bmc_user.new_password, role)
                .await?;
        }
        DeleteBmcUser(bmc_username) => {
            redfish.delete_user(&bmc_username.user).await?;
        }
        ChangeBmcUsername(bmc_username) => {
            redfish
                .change_username(&bmc_username.old_user, &bmc_username.new_user)
                .await?;
            println!(
                "User {} renamed to {}",
                bmc_username.old_user, bmc_username.new_user
            );
        }
        ChangeBmcPassword(bmc_password) => {
            redfish
                .change_password(&bmc_password.user, &bmc_password.new_password)
                .await?;
            println!("BIOS settings changes require system restart");
        }
        ChangeUefiPassword(uefi_password) => {
            redfish
                .change_uefi_password(&uefi_password.current_password, &uefi_password.new_password)
                .await?;
            println!("BIOS settings changes require system restart");
        }
        Dpu(dpu) => match dpu {
            DpuOperations::Firmware(fw) => match fw {
                FwCommand::Status => {
                    handle_fw_status(redfish).await?;
                }
                FwCommand::Update(fw_package) => {
                    let file_result = tokio::fs::File::open(fw_package.package).await;
                    if let Ok(file) = file_result {
                        redfish.update_firmware(file).await?;
                        println!("Track update progress using firmware status");
                    } else if let Err(err) = file_result {
                        eprintln!("Error opening file: {err}");
                    }
                }
                FwCommand::Show(fw) => {
                    handle_fw_show(redfish, fw).await?;
                }
            },
            DpuOperations::Ports(ports) => {
                handle_port_show(redfish, ports).await?;
            }
        },
        GetChassisAll => {
            handle_get_chassis_all(redfish).await?;
        }
        GetChassis(chassis) => {
            handle_get_chassis(redfish, chassis.chassis_id).await?;
        }
        GetBmcEthernetInterfaces => {
            handle_ethernet_interface_show(redfish, false).await?;
        }
        GetSystemEthernetInterfaces => {
            handle_ethernet_interface_show(redfish, true).await?;
        }
        GetManager => {
            let manager = redfish.get_manager().await?;
            println!("{manager:#?}");
        }
        UpdateFirmwareMultipart(details) => {
            let component_type = match details.component_type {
                Some(x) => x,
                None => ComponentType::Unknown,
            };
            let taskid = redfish
                .update_firmware_multipart(
                    Path::new(&details.filename),
                    true,
                    Duration::from_secs(120),
                    component_type,
                )
                .await?;
            loop {
                let task = redfish.get_task(&taskid).await?;
                match &task.task_state {
                    None => {
                        println!("No task state");
                        return Ok(());
                    }
                    Some(task_state) => match task_state {
                        TaskState::New => {
                            println!("Task has not yet started: {task:?}");
                        }
                        TaskState::Starting => {
                            println!("Task is starting: {task:?}");
                        }
                        TaskState::Pending => {
                            println!("Task is starting: {task:?}");
                        }
                        TaskState::Running => {
                            println!("Task is running: {task:?}");
                        }
                        TaskState::Completed => {
                            println!("Task is complete: {task:?}");
                            return Ok(());
                        }
                        _ => {
                            println!("Bad task state: {task:?}");
                            return Ok(());
                        }
                    },
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
            }
        }
        GetTasks => {
            let tasks = redfish.get_tasks().await?;
            let mut table = Table::new();
            table.add_row(row!["id"]);
            for task in &tasks {
                table.add_row(row![task]);
            }
            table.printstd();
        }
        GetTask(details) => {
            let task = redfish.get_task(&details.taskid).await?;
            println!("Task info: {task:?}");
        }
        ClearUefiPassword(uefi_password) => {
            let job = redfish
                .clear_uefi_password(&uefi_password.current_password)
                .await?;
            println!("Clear UEFI password Job ID: {:?}", job.unwrap_or_default());
        }
        IsIpmiOverLanEnabled => {
            let status = redfish.is_ipmi_over_lan_enabled().await?;
            println!("IPMI enabled over LAN: {status}");
        }
        EnableIpmiOverLan => {
            redfish
                .enable_ipmi_over_lan(EnabledDisabled::Enabled)
                .await?;
        }
        DisableIpmiOverLan => {
            redfish
                .enable_ipmi_over_lan(EnabledDisabled::Disabled)
                .await?;
        }
        GetBaseMacAddress => {
            let mut base_mac = redfish.get_base_mac_address().await?.unwrap_or_default();
            base_mac = base_mac.replace('"', "");
            println!("Raw Mac Address: {base_mac}, length: {}", base_mac.len());
            base_mac.insert(10, ':');
            base_mac.insert(8, ':');
            base_mac.insert(6, ':');
            base_mac.insert(4, ':');
            base_mac.insert(2, ':');
            match MacAddress::from_str(base_mac.as_str()) {
                Ok(mac) => {
                    println!("Parsed Base Mac Address: {mac}");
                }
                Err(e) => {
                    println!("failed to parse mac address from {base_mac}: {e}");
                }
            }
        }
        ClearNvram => {
            redfish.clear_nvram().await?;
        }
        SetBios(set_bios) => {
            let attrmap: HashMap<String, serde_json::Value> =
                serde_json::from_str(set_bios.attributes.as_str())?;
            redfish.set_bios(attrmap).await?;
            println!("success");
        }
        ResetBios(args) => {
            redfish.reset_bios().await?;
            if args.reboot {
                redfish.power(SystemPowerControl::ForceRestart).await?;
            } else {
                println!("BIOS changes require a system restart to take effect.");
            }
        }
        GetNicMode => {
            let is_dpu_in_nic_mode = redfish.get_nic_mode().await?;
            println!("{is_dpu_in_nic_mode:#?}");
        }
        IsInfiniteBootEnabled => {
            let is_infinite_boot_enabled = redfish.is_infinite_boot_enabled().await?;
            match is_infinite_boot_enabled {
                Some(true) => println!("Enabled"),
                Some(false) => println!("Disabled"),
                None => println!("Unknown"),
            }
        }
        EnableInfiniteBoot => {
            redfish.enable_infinite_boot().await?;
            println!("BIOS changes require a system restart to take effect.");
        }
        SetNicMode => {
            redfish.set_nic_mode(NicMode::Nic).await?;
        }
        SetDpuMode => {
            redfish.set_nic_mode(NicMode::Dpu).await?;
        }
        ChassisResetCard1Powercycle => {
            redfish
                .chassis_reset("Card1", SystemPowerControl::PowerCycle)
                .await?;
        }
        SetBootOrderDpuFirst(args) => {
            let boot_interface = libredfish::BootInterfaceRef::Mac(
                args.boot_interface_mac
                    .parse::<MacAddress>()
                    .map_err(|e| eyre!("invalid boot_interface_mac: {e}"))?,
            );
            if let Some(job_id) = redfish.set_boot_order_dpu_first(boot_interface).await? {
                tracing::info!(
                    "successfully configured BIOS job {job_id} to set {} first in the server's boot order",
                    args.boot_interface_mac
                )
            } else {
                tracing::info!(
                    "successfully set {} first in the server's boot order",
                    args.boot_interface_mac
                )
            }
        }
        GetHostRshim => {
            if let Some(enabled_value) = redfish.get_host_rshim().await? {
                tracing::info!("Host RSHIM is {}", enabled_value);
            } else {
                tracing::info!("Host RSHIM is None");
            }
        }
        EnableHostRshim => {
            redfish.set_host_rshim(EnabledDisabled::Enabled).await?;
        }
        DisableHostRshim => {
            redfish.set_host_rshim(EnabledDisabled::Disabled).await?;
        }
        GetBossController => {
            if let Some(controller_id) = redfish.get_boss_controller().await? {
                tracing::info!("BOSS Controller ID: {}", controller_id);
            } else {
                tracing::info!("Did not find BOSS Controller");
            }
        }
        DecommissionController(args) => {
            if let Some(jid) = redfish
                .decommission_storage_controller(&args.controller_id)
                .await?
            {
                tracing::info!("JID: {}", jid);
            } else {
                tracing::info!("No JID");
            }
        }
        CreateVolume(args) => {
            if let Some(jid) = redfish
                .create_storage_volume(&args.controller_id, &args.volume_name)
                .await?
            {
                tracing::info!("JID: {}", jid);
            } else {
                tracing::info!("No JID");
            }
        }
        IsBootOrderSetup(args) => {
            let boot_interface = libredfish::BootInterfaceRef::Mac(
                args.boot_interface_mac
                    .parse::<MacAddress>()
                    .map_err(|e| eyre!("invalid boot_interface_mac: {e}"))?,
            );
            let setup = redfish.is_boot_order_setup(boot_interface).await?;
            tracing::info!(setup);
        }
    }
    Ok(())
}

async fn handle_fw_status(redfish: Box<dyn Redfish>) -> Result<(), RedfishError> {
    let tasks: Vec<String> = redfish.get_tasks().await?;
    let mut tasks_info: Vec<Task> = Vec::new();
    for task in tasks.iter() {
        if let Ok(t) = redfish.get_task(task).await {
            tasks_info.push(t);
        } else {
            println!("Task {task} not found.");
        }
    }
    convert_tasks_to_nice_table(tasks_info).printstd();
    Ok(())
}

async fn handle_fw_show(redfish: Box<dyn Redfish>, args: ShowFw) -> Result<(), RedfishError> {
    if args.all || args.bmc || args.dpu_os || args.uefi || args.fw.is_empty() {
        let f = FwFilter {
            only_bmc: args.bmc,
            only_dpu_os: args.dpu_os,
            only_uefi: args.uefi,
        };
        match show_all_fws(redfish, f).await {
            Ok(_) => {
                // TODO(chet): Remove this ~March 2024.
                // Use tracing::warn for this so its both a little more
                // noticeable, and a little more annoying/naggy. If people
                // complain, it means its working.
                if args.all {
                    warn!("redundant `--all` with basic `show` is deprecated. just do `fw show`")
                }
                Ok(())
            }
            Err(e) => {
                eprintln!("Error displaying firmware information: {e}");
                Err(e)
            }
        }
    } else {
        match redfish.get_firmware(&args.fw).await {
            Ok(firmware) => {
                convert_fws_to_nice_table(vec![firmware]).printstd();
                Ok(())
            }
            Err(err) => {
                eprintln!("Error fetching firmware '{}'", args.fw);
                Err(err)
            }
        }
    }
}

#[derive(Debug, Default, Copy, Clone)]
struct FwFilter {
    only_bmc: bool,
    only_dpu_os: bool,
    only_uefi: bool,
}

/// True if a firmware-inventory id refers to the BMC firmware. BF3 exposes it
/// as `"BMC_Firmware"`; BF4 uses exactly `"BlueField_FW_BMC_0"`. Matching the
/// full BF4 id (via `ends_with`, allowing an optional path prefix) excludes
/// unrelated components — including `"..._FW_BMC_0_x"` / `"..._FW_BMC_01"` and
/// any other id that merely ends in `"FW_BMC_0"`.
fn is_bmc_firmware_id(id: &str) -> bool {
    id.contains("BMC_Firmware") || id.ends_with("BlueField_FW_BMC_0")
}

async fn show_all_fws(redfish: Box<dyn Redfish>, f: FwFilter) -> Result<(), RedfishError> {
    let fws: Vec<String> = redfish.get_software_inventories().await?;
    let mut fws_info: Vec<SoftwareInventory> = Vec::new();

    for fw in fws.iter() {
        if let Ok(firmware) = redfish.get_firmware(fw).await {
            fws_info.push(firmware);
        } else {
            println!("Firmware {fw} not found.");
        }
    }

    if f.only_bmc {
        if !fws.iter().any(|id| is_bmc_firmware_id(id)) {
            println!("BMC FW is not found");
            return Err(RedfishError::NoContent);
        }
        fws_info.retain(|f| is_bmc_firmware_id(&f.id));
    }

    if f.only_dpu_os {
        if !fws.contains(&"DPU_OS".to_string()) {
            println!("DPU OS is not found");
            return Err(RedfishError::NoContent);
        }
        fws_info.retain(|f| f.id == *"DPU_OS");
    }

    if f.only_uefi {
        if !fws.contains(&"DPU_UEFI".to_string()) {
            println!("DPU UEFI is not found");
            return Err(RedfishError::NoContent);
        }
        fws_info.retain(|f| f.id == *"DPU_UEFI");
    }

    convert_fws_to_nice_table(fws_info).printstd();
    Ok(())
}

fn convert_fws_to_nice_table(fws: Vec<SoftwareInventory>) -> Box<Table> {
    let mut table = Table::new();
    table.add_row(row!["Id", "Version"]);
    for fw in fws {
        table.add_row(row![fw.id, fw.version.unwrap_or("(NULL)".to_string())]);
    }
    table.into()
}

fn convert_tasks_to_nice_table(tasks: Vec<Task>) -> Box<Table> {
    let mut table = Table::new();
    table.add_row(row![
        "Id",
        "State",
        "Status",
        "Percentage Completed (%)",
        "Messages"
    ]);
    for mut task in tasks {
        table.add_row(row![
            task.id,
            task.task_state
                .unwrap_or(libredfish::model::task::TaskState::Exception)
                .to_string(),
            task.task_status.unwrap_or("None".to_string()),
            task.percent_complete.unwrap_or(0),
            task.messages
                .pop()
                .map(|last_message| last_message.message)
                .unwrap_or("No Message".to_string())
        ]);
    }
    table.into()
}

async fn handle_port_show(redfish: Box<dyn Redfish>, args: ShowPort) -> Result<(), RedfishError> {
    match show_all_ports(redfish).await {
        Ok((mut ports_info, netdev_funcs_info)) => {
            if !args.port.is_empty() {
                ports_info.retain(|f| f.id.as_ref().is_some_and(|id| id == &args.port));
            }
            convert_ports_to_nice_table(ports_info, netdev_funcs_info).printstd();
            // TODO(chet): Remove this ~March 2024.
            // Use tracing::warn for this so its both a little more
            // noticeable, and a little more annoying/naggy. If people
            // complain, it means its working.
            if args.all {
                warn!("redundant `--all` with basic `show` is deprecated. just do `port show`")
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

async fn get_bluefield_chassis(redfish: &dyn Redfish) -> Result<String, RedfishError> {
    let chassis_vec: Vec<String> = redfish.get_chassis_all().await?;
    if let Some(bluefield_bmc) = chassis_vec
        .iter()
        .find(|&chassis| chassis.contains("Bluefield"))
    {
        Ok(bluefield_bmc.to_string())
    } else {
        eprintln!("Bluefield chassis was not found");
        Err(RedfishError::NoContent)
    }
}

async fn show_all_ports(
    redfish: Box<dyn Redfish>,
) -> Result<(Vec<NetworkPort>, Vec<NetworkDeviceFunction>), RedfishError> {
    let chassis_id = get_bluefield_chassis(&*redfish).await?;
    let ports: Vec<String> = redfish
        .get_ports(&chassis_id, "NvidiaNetworkAdapter")
        .await?;
    let mut ports_info: Vec<NetworkPort> = Vec::new();

    for p in ports.iter() {
        match redfish
            .get_port(&chassis_id, "NvidiaNetworkAdapter", p)
            .await
        {
            Ok(port) => {
                ports_info.push(port);
            }
            Err(err) => {
                eprintln!("Error fetching port {p}: {err}");
            }
        }
    }

    let netdev_funcs: Vec<String> = redfish.get_network_device_functions(&chassis_id).await?;
    let mut netdev_funcs_info: Vec<NetworkDeviceFunction> = Vec::new();

    for n in netdev_funcs.iter() {
        match redfish
            .get_network_device_function(&chassis_id, n, None)
            .await
        {
            Ok(netdev) => {
                netdev_funcs_info.push(netdev);
            }
            Err(err) => {
                eprintln!("Error fetching network device {n}: {err}");
            }
        }
    }
    Ok((ports_info, netdev_funcs_info))
}

fn convert_ports_to_nice_table(
    ports: Vec<NetworkPort>,
    netdevs: Vec<NetworkDeviceFunction>,
) -> Box<Table> {
    let mut table = Table::new();
    table.add_row(row![
        "Id",
        "Link Status",
        "Type",
        "MAC Address",
        "MTU Size",
        "Speed (Gbps)"
    ]);

    for port in &ports {
        for netdev in &netdevs {
            if let (Some(port_id), Some(netdev_id)) = (&port.id, &netdev.id)
                && netdev_id.contains(port_id)
            {
                table.add_row(row![
                    port_id,
                    port.link_status
                        .as_ref()
                        .unwrap_or(&LinkStatus::NoLink)
                        .to_string(),
                    netdev
                        .net_dev_func_type
                        .as_ref()
                        .unwrap_or(&"None".to_string()),
                    netdev
                        .ethernet
                        .as_ref()
                        .and_then(|ethernet| ethernet.mac_address.as_ref())
                        .cloned()
                        .unwrap_or_else(|| "None".to_string()),
                    netdev
                        .ethernet
                        .as_ref()
                        .and_then(|ethernet| ethernet.mtu_size)
                        .unwrap_or(0),
                    port.current_speed_gbps.unwrap_or(0.0),
                ]);
            }
        }
    }

    table.into()
}

async fn handle_ethernet_interface_show(
    redfish: Box<dyn Redfish>,
    fetch_system_interfaces: bool,
) -> Result<(), RedfishError> {
    let eth_ifs: Vec<String> = match fetch_system_interfaces {
        false => redfish.get_manager_ethernet_interfaces().await?,
        true => redfish.get_system_ethernet_interfaces().await?,
    };
    let mut eth_ifs_info: Vec<EthernetInterface> = Vec::new();

    for iface_id in eth_ifs.iter() {
        let result = match fetch_system_interfaces {
            false => redfish.get_manager_ethernet_interface(iface_id).await,
            true => redfish.get_system_ethernet_interface(iface_id).await,
        };

        match result {
            Ok(iface) => {
                eth_ifs_info.push(iface);
            }
            Err(err) => {
                eprintln!("Error fetching ethernet interface '{iface_id}': {err}");
            }
        }
    }
    convert_ethernet_interfaces_to_nice_table(eth_ifs_info).printstd();
    Ok(())
}

fn convert_ethernet_interfaces_to_nice_table(eth_ifs: Vec<EthernetInterface>) -> Box<Table> {
    let mut table = Table::new();
    table.add_row(row![
        "Id",
        "Link Status",
        "MAC Address",
        "IP Addresses",
        "Static IP Addresses",
        "MTU Size",
        "Speed (Mbps)",
        "UefiDevicePath"
    ]);

    for eth_if in eth_ifs {
        let mut ips = Vec::new();
        for ip in eth_if.ipv4_addresses {
            if let Some(ip) = ip.address {
                ips.push(ip);
            }
        }
        for ip in eth_if.ipv6_addresses {
            if let Some(ip) = ip.address {
                ips.push(ip);
            }
        }

        let mut static_ips = Vec::new();
        for ip in eth_if.ipv4_static_addresses {
            if let Some(ip) = ip.address {
                static_ips.push(ip);
            }
        }
        for ip in eth_if.ipv6_static_addresses {
            if let Some(ip) = ip.address {
                static_ips.push(ip);
            }
        }

        table.add_row(row![
            eth_if.id.as_ref().unwrap_or(&"None".to_string()),
            eth_if.link_status.unwrap_or(LinkStatus::NoLink).to_string(),
            eth_if.mac_address.as_deref().unwrap_or("None"),
            ips.join(","),
            static_ips.join(","),
            eth_if
                .mtu_size
                .map(|mtu| mtu.to_string())
                .as_deref()
                .unwrap_or("N/A"),
            eth_if
                .speed_mbps
                .map(|s| s.to_string())
                .as_deref()
                .unwrap_or("N/A"),
            eth_if.uefi_device_path.as_deref().unwrap_or("N/A")
        ]);
    }
    table.into()
}

async fn handle_get_chassis_all(redfish: Box<dyn Redfish>) -> Result<(), RedfishError> {
    let chassis_vec: Vec<String> = redfish.get_chassis_all().await?;
    let mut chassis_info: Vec<Chassis> = Vec::new();

    for c in chassis_vec.iter() {
        match redfish.get_chassis(c).await {
            Ok(chassis) => {
                chassis_info.push(chassis);
            }
            Err(err) => {
                eprintln!("Error fetching chassis '{c}': {err}");
            }
        }
    }
    convert_chassis_to_nice_table(chassis_info).printstd();
    Ok(())
}

async fn handle_get_chassis(
    redfish: Box<dyn Redfish>,
    chassis_id: String,
) -> Result<(), RedfishError> {
    let chassis_vec: Vec<String> = redfish.get_chassis_all().await?;
    for c in chassis_vec.iter() {
        match redfish.get_chassis(c).await {
            Ok(chassis) => {
                if *c == chassis_id {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&chassis).unwrap_or_default()
                    );
                    return Ok(());
                }
            }
            Err(err) => {
                eprintln!("Error fetching chassis '{c}': {err}");
            }
        }
    }

    println!("Could not find chassis with id {chassis_id} out of {chassis_vec:#?}");
    Ok(())
}

fn convert_chassis_to_nice_table(chassis_vec: Vec<Chassis>) -> Box<Table> {
    let mut table = Table::new();
    table.add_row(row!["Id", "Manufacturer", "Model",]);

    for chassis in &chassis_vec {
        table.add_row(row![
            chassis.id.as_ref().unwrap_or(&"None".to_string()),
            chassis.manufacturer.as_ref().unwrap_or(&"None".to_string()),
            chassis.model.as_ref().unwrap_or(&"None".to_string()),
        ]);
    }
    table.into()
}

#[cfg(test)]
mod tests {
    use super::is_bmc_firmware_id;

    #[test]
    fn is_bmc_firmware_id_matches_bf3_and_bf4_only() {
        // BF3 id and BF4 id are recognized.
        assert!(is_bmc_firmware_id("BMC_Firmware"));
        assert!(is_bmc_firmware_id("BlueField_FW_BMC_0"));

        // Not the BMC firmware: unrelated components, slot/backup variants, or
        // any id that merely ends in "FW_BMC_0".
        assert!(!is_bmc_firmware_id("DPU_OS"));
        assert!(!is_bmc_firmware_id("BlueField_FW_BMC_0_ERoT"));
        assert!(!is_bmc_firmware_id("BlueField_FW_BMC_01"));
        assert!(!is_bmc_firmware_id("Something_FW_BMC_0"));
    }
}
