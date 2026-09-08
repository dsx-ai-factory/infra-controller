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
use std::net::IpAddr;

use ::rpc::forge as rpc;
use carbide_ipxe_renderer::{
    DefaultIpxeScriptRenderer, IpxeScript, IpxeScriptRenderer, IpxeTemplateArtifact,
    IpxeTemplateArtifactCacheStrategy, IpxeTemplateParameter,
};
use carbide_uuid::machine::{MachineId, MachineInterfaceId, MachineType};
use db::{self};
use mac_address::MacAddress;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    DpuInitState, FailureCause, FailureDetails, HostReprovisionState, InstanceState,
    ManagedHostState, MeasuringState, ReprovisionState, ValidationState,
};
use sqlx::PgConnection;

use crate::CarbideError;

pub(crate) struct PxeInstructionRequest {
    pub(crate) arch: rpc::MachineArchitecture,
    pub(crate) product: Option<String>,
    pub(crate) client_ip: IpAddr,
}

/// Input provided to `PxeInstructions::get_pxe_instructions`.
/// The PxeInstructionsRequest model contains the client_ip
/// as determined by carbide-pxe, whereas PxeInstructionsInput
/// contains the resolved machine_interface_id.
pub(crate) struct PxeInstructionsInput {
    pub(crate) interface_id: MachineInterfaceId,
    pub(crate) arch: rpc::MachineArchitecture,
    pub(crate) product: Option<String>,
}

impl TryFrom<rpc::PxeInstructionRequest> for PxeInstructionRequest {
    type Error = ::rpc::errors::RpcDataConversionError;

    fn try_from(value: rpc::PxeInstructionRequest) -> Result<Self, Self::Error> {
        let arch = rpc::MachineArchitecture::try_from(value.arch)
            .map_err(|_| Self::Error::InvalidArgument("Unknown arch received.".to_string()))?;

        let client_ip_str = value
            .client_ip
            .ok_or(Self::Error::MissingArgument("client_ip"))?;
        let client_ip: IpAddr = client_ip_str.parse().map_err(|e| {
            Self::Error::InvalidArgument(format!("Failed parsing client_ip '{client_ip_str}': {e}"))
        })?;

        let product = value.product;

        Ok(PxeInstructionRequest {
            arch,
            product,
            client_ip,
        })
    }
}

/// Converts an operating_systems row (type ipxe_os_definition) to IpxeScript for the renderer.
fn operating_system_row_to_ipxe_script(
    row: &db::operating_system::OperatingSystem,
) -> Result<IpxeScript, CarbideError> {
    if row.type_ != model::operating_system_definition::OS_TYPE_TEMPLATED_IPXE {
        return Err(CarbideError::internal(format!(
            "operating_system {} has type {}, expected {}",
            row.id,
            row.type_,
            model::operating_system_definition::OS_TYPE_TEMPLATED_IPXE,
        )));
    }
    let ipxe_template_id = row.ipxe_template_id.clone().unwrap_or_default();
    let parameters: Vec<IpxeTemplateParameter> = row
        .ipxe_parameters
        .as_ref()
        .and_then(|j| {
            j.0.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        let obj = v.as_object()?;
                        Some(IpxeTemplateParameter {
                            name: obj.get("name")?.as_str()?.to_string(),
                            value: obj.get("value")?.as_str().unwrap_or("").to_string(),
                        })
                    })
                    .collect()
            })
        })
        .unwrap_or_default();
    let artifacts: Vec<IpxeTemplateArtifact> = row
        .ipxe_artifacts
        .as_ref()
        .and_then(|j| {
            j.0.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        let obj = v.as_object()?;
                        let cache_strategy = match obj
                            .get("cache_strategy")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0)
                        {
                            1 => IpxeTemplateArtifactCacheStrategy::LocalOnly,
                            2 => IpxeTemplateArtifactCacheStrategy::CachedOnly,
                            3 => IpxeTemplateArtifactCacheStrategy::RemoteOnly,
                            _ => IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
                        };
                        Some(IpxeTemplateArtifact {
                            name: obj.get("name")?.as_str()?.to_string(),
                            url: obj.get("url")?.as_str().unwrap_or("").to_string(),
                            sha: obj.get("sha").and_then(|v| v.as_str()).map(String::from),
                            auth_type: obj
                                .get("auth_type")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                            auth_token: obj
                                .get("auth_token")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                            cache_strategy,
                            cached_url: obj
                                .get("cached_url")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                        })
                    })
                    .collect()
            })
        })
        .unwrap_or_default();
    Ok(IpxeScript {
        name: row.name.clone(),
        description: row.description.clone(),
        hash: row.ipxe_definition_hash.clone().unwrap_or_default(),
        tenant_id: row.org.clone(),
        ipxe_template_id,
        parameters,
        artifacts,
    })
}

pub(crate) struct PxeInstructions;

#[derive(serde::Serialize)]
struct InstructionGenerator {
    kernel: String,
    command_line: String,
    initrd: Option<String>,
}

impl InstructionGenerator {
    fn serialize_pxe_instructions(&self) -> String {
        match &self.initrd {
            Some(initrd) => {
                format!(
                    r#"
kernel {} initrd=initrd.img {} ||
imgfetch --name initrd.img {} ||
boot ||
"#,
                    self.kernel, self.command_line, initrd
                )
            }
            None => {
                let output = format!(
                    r#"
kernel {} {} ||
boot ||
"#,
                    self.kernel, self.command_line
                );
                output
            }
        }
    }
}

impl PxeInstructions {
    fn get_pxe_instruction_for_arch(
        arch: rpc::MachineArchitecture,
        machine_interface_id: MachineInterfaceId,
        mac_address: MacAddress,
        console: &str,
        machine_type: MachineType,
    ) -> String {
        tracing::info!(
            machine_type = %machine_type,
            machine_interface_id = %machine_interface_id,
            mac_address = %mac_address,
            "machine network boot parameters",
        );
        match arch {
            rpc::MachineArchitecture::Arm => {
                if machine_type == MachineType::Host || machine_type == MachineType::PredictedHost {
                    InstructionGenerator {
                        kernel: "${base-url}/internal/aarch64/scout.efi".to_string(),
                        command_line: format!("mac={mac_address} console=tty0 console={console},115200 pci=realloc=off iommu=off cli_cmd=auto-detect machine_id={machine_interface_id} server_uri=[api_url] pxe_uri=[pxe_url] ds=nocloud;s=${{scout-cloudinit-url}}"),
                        initrd: None,
                    }
                }
                else {
                     // For the DPUs, bfks => BlueField Kick Start script
                     InstructionGenerator {
                        kernel: "${base-url}/internal/aarch64/carbide.efi".to_string(),
                        command_line: format!("console=tty0 console=ttyS0,115200 console=ttyAMA0 console=hvc0 ip=dhcp cli_cmd=auto-detect bfnet=oob_net0:dhcp bfks=${{dpu-cloudinit-url}}/user-data machine_id={machine_interface_id} server_uri=[api_url] pxe_uri=[pxe_url]"),
                        initrd: Some("${base-url}/internal/aarch64/carbide.root".to_string()),
                    }
                }
            }
            rpc::MachineArchitecture::X86 => {
                InstructionGenerator {
                    kernel: "${base-url}/internal/x86_64/scout.efi".to_string(),
                    command_line: format!("mac={mac_address} console=tty0 console={console},115200 pci=realloc=off iommu=off cli_cmd=auto-detect machine_id={machine_interface_id} server_uri=[api_url] pxe_uri=[pxe_url] ds=nocloud;s=${{scout-cloudinit-url}}"),
                    initrd: None,
                }
            }
        }.serialize_pxe_instructions()
    }

    /// Render an IpxeScript definition using the template-based renderer
    fn render_ipxe_script(
        ipxeos: &IpxeScript,
        base_url: &str,
        console: &str,
    ) -> Result<String, CarbideError> {
        let renderer = DefaultIpxeScriptRenderer::new();

        let mut reserved_params = Vec::new();

        if let Some(template) = renderer.get_template_by_id(&ipxeos.ipxe_template_id) {
            if template
                .reserved_params
                .iter()
                .any(|p| p.to_lowercase() == "base_url")
            {
                reserved_params.push(IpxeTemplateParameter {
                    name: "base_url".to_string(),
                    value: base_url.to_string(),
                });
            }

            if template
                .reserved_params
                .iter()
                .any(|p| p.to_lowercase() == "console")
            {
                reserved_params.push(IpxeTemplateParameter {
                    name: "console".to_string(),
                    value: console.to_string(),
                });
            }
        }

        renderer
            .render(ipxeos, &reserved_params)
            .map_err(|e| CarbideError::internal(format!("failed to render iPXE script: {}", e)))
    }

    pub(crate) async fn get_pxe_instructions(
        txn: &mut PgConnection,
        target: PxeInstructionsInput,
    ) -> Result<String, CarbideError> {
        let error_instructions = |machine_id: MachineId,
                                  interface_id: MachineInterfaceId,
                                  state: &ManagedHostState|
         -> String {
            format!(
                r#"
echo Machine ID: {machine_id}
echo Interface ID: {interface_id}
echo Current state: {state}
echo Could not continue boot due to invalid state ||
sleep 5 ||
exit ||
"#
            )
        };

        let exit_instructions = |machine_id: MachineId,
                                 interface_id: MachineInterfaceId,
                                 state: &ManagedHostState|
         -> String {
            format!(
                r#"
echo Machine ID: {machine_id}
echo Interface ID: {interface_id}
echo Current state: {state}
echo This state assumes an OS is provisioned and will exit into the OS in 5 seconds. To re-run iPXE instructions and OS installation, trigger a reboot request with flag rebootWithCustomIpxe/boot_with_custom_ipxe set. ||
sleep 5 ||
exit ||
"#
            )
        };

        static UNKNOWN_HOST_INSTRUCTIONS: &str = r#"
echo this is an unknown host interface, not PXE booting ||
sleep 5 ||
exit ||
        "#;

        let mut console = "ttyS0";
        let mut qcow_imager_url = "chain ${base-url}/internal/x86_64/qcow-imager.efi loglevel=7 console=tty0 pci=realloc=off ";
        let interface = db::machine_interface::find_one(&mut *txn, target.interface_id).await?;

        // This custom pxe is different from a customer instance of pxe. It is more for testing one off
        // changes until a real dev env is established and we can just override our existing code to test
        // It is possible for the pxe to be null if we are only trying to test the user data, and this will
        // follow the same code path and retrieve the non customer pxe
        if let Some(machine_boot_override) =
            db::machine_boot_override::find_optional(&mut *txn, target.interface_id).await?
            && let Some(custom_pxe) = machine_boot_override.custom_pxe
        {
            return Ok(custom_pxe);
        }

        let Some(machine_id) = interface.machine_id else {
            // FORGE-7330: If site-explorer can't get the interface info from the bmc, then it won't associate the interface with a machine.
            // if the pxe request included the product and its a DPU, the machine record is not needed and we can just use the DPU type.
            if let Some(product) = target.product
                && product.to_ascii_lowercase().contains("bluefield")
            {
                if target.arch == rpc::MachineArchitecture::Arm {
                    return Ok(PxeInstructions::get_pxe_instruction_for_arch(
                        target.arch,
                        target.interface_id,
                        interface.mac_address,
                        console,
                        MachineType::Dpu,
                    ));
                } else {
                    tracing::warn!(
                        product = %product,
                        arch = ?target.arch,
                        "Unsupported DPU type",
                    )
                }
            };
            // We haven't minted a machine ID for this yet, so we don't know if it's a DPU or a
            // Host. We can't assume ARM = DPU, because there are zero-dpu ARM hosts. Heuristics we
            // use:
            // - If we don't have an exploration report for this MAC address, don't PXE boot at all
            // - If it's X86 and we have an exploration report, assume it's a Host.
            // - If it's ARM, only treat it as a DPU when the explored endpoint is itself a DPU BMC.
            let Some(endpoint) =
                db::explored_endpoints::find_by_mac_address(&mut *txn, interface.mac_address)
                    .await?
                    .into_iter()
                    .next()
            else {
                // This only happens if someone powered on a host manually before we ingested it,
                // which is unlikely but possible.
                tracing::info!(machine_interface = ?interface, "Request for PXE instructions for unknown interface, skipping PXE boot");
                return Ok(UNKNOWN_HOST_INSTRUCTIONS.to_string());
            };

            let (machine_type, console) = match target.arch {
                rpc::MachineArchitecture::X86 => (MachineType::PredictedHost, console),
                rpc::MachineArchitecture::Arm => {
                    if endpoint.report.is_dpu() {
                        (MachineType::Dpu, console)
                    } else {
                        (MachineType::PredictedHost, "ttyAMA0")
                    }
                }
            };

            return Ok(PxeInstructions::get_pxe_instruction_for_arch(
                target.arch,
                target.interface_id,
                interface.mac_address,
                console,
                machine_type,
            ));
        };

        let machine = db::machine::find_one(&mut *txn, &machine_id, MachineSearchConfig::default())
            .await
            .map_err(|e| CarbideError::InvalidArgument(format!("get machine failed, error: {e}")))?
            .ok_or(CarbideError::InvalidArgument(
                "invalid machine id. not found in db".to_string(),
            ))?;

        tracing::info!(machine_id = %machine.id, machine_interface_id = %target.interface_id, machine_state = %machine.current_state(), "Found existing machine for pxe instructions");
        // DPUs need to boot twice during initial discovery. Both reboots require
        // that the DPU gets pxe instructions.
        //
        // The first boot (before it even exists in the DB) enables firmware update
        // but will not install HBN.  This is handled above when no machine is found.
        //
        // The second boot enables HBN.  This is handled here when the DPU is
        // waiting for the network install
        if let Ok(dpu_machine_id) = machine.dpu_machine_id() {
            if let Some(reprov_state) = &machine
                .current_state()
                .as_reprovision_state(&dpu_machine_id)
                && matches!(
                    reprov_state,
                    ReprovisionState::FirmwareUpgrade | ReprovisionState::WaitingForNetworkInstall
                )
            {
                return Ok(PxeInstructions::get_pxe_instruction_for_arch(
                    target.arch,
                    target.interface_id,
                    interface.mac_address,
                    console,
                    machine.id.machine_type(),
                ));
            }

            match &machine.current_state() {
                ManagedHostState::DPUInit { dpu_states } => {
                    let Some(dpu_state) = dpu_states.states.get(&dpu_machine_id) else {
                        return Err(CarbideError::MissingDpu(machine_id));
                    };

                    match dpu_state {
                        DpuInitState::Init => {
                            return Ok(PxeInstructions::get_pxe_instruction_for_arch(
                                target.arch,
                                target.interface_id,
                                interface.mac_address,
                                console,
                                machine.id.machine_type(),
                            ));
                        }
                        _ => {
                            return Ok(exit_instructions(
                                machine_id,
                                target.interface_id,
                                machine.current_state(),
                            ));
                        }
                    }
                }
                _ => {
                    return Ok(exit_instructions(
                        machine_id,
                        target.interface_id,
                        machine.current_state(),
                    ));
                }
            }
        }

        if target.arch == rpc::MachineArchitecture::Arm {
            console = "ttyAMA0";
            qcow_imager_url = "chain ${base-url}/internal/aarch64/qcow-imager.efi loglevel=7 console=tty0 pci=realloc=off ";
        } else if let Some(hardware_info) = machine.status.hardware_info.as_ref()
            && let Some(dmi_info) = hardware_info.dmi_data.as_ref()
            && (dmi_info.sys_vendor == "Lenovo" || dmi_info.sys_vendor == "Supermicro")
        {
            console = "ttyS1";
        }

        let pxe_script = match &machine.current_state() {
            ManagedHostState::Ready
            | ManagedHostState::HostInit { .. }
            | ManagedHostState::Decommissioning { .. }
            | ManagedHostState::BootConfiguring { .. }
            | ManagedHostState::BomValidating { .. }
            | ManagedHostState::Measuring {
                measuring_state: MeasuringState::WaitingForMeasurements,
            }
            | ManagedHostState::Failed {
                details:
                    FailureDetails {
                        cause: FailureCause::Discovery { .. },
                        ..
                    },
                ..
            }
            | ManagedHostState::Failed {
                details:
                    FailureDetails {
                        cause: FailureCause::NVMECleanFailed { .. },
                        ..
                    },
                ..
            }
            | ManagedHostState::Failed {
                details:
                    FailureDetails {
                        cause: FailureCause::MachineValidation { .. },
                        ..
                    },
                ..
            }
            | ManagedHostState::Validation {
                validation_state: ValidationState::MachineValidation { .. },
            }
            | ManagedHostState::WaitingForCleanup { .. } => Self::get_pxe_instruction_for_arch(
                target.arch,
                target.interface_id,
                interface.mac_address,
                console,
                machine.id.machine_type(),
            ),
            ManagedHostState::Assigned { instance_state } => match instance_state {
                InstanceState::Ready => {
                    let instance = db::instance::find_by_machine_id(txn, &machine_id)
                        .await?
                        .ok_or(CarbideError::NotFoundError {
                            kind: "machine",
                            id: machine_id.to_string(),
                        })?;

                    if instance
                        .config
                        .os
                        .run_provisioning_instructions_on_every_boot
                        || instance.use_custom_pxe_on_boot
                    {
                        let retry_on_failure = instance.use_custom_pxe_on_boot;

                        // Clear the flag now that we're serving this one-time request. Instances
                        // configured for every boot continue through
                        // `run_provisioning_instructions_on_every_boot`.
                        if retry_on_failure {
                            db::instance::use_custom_ipxe_on_next_boot(&machine_id, false, txn)
                                .await?;
                        }

                        let provisioning_script = match instance.config.os.variant {
                            model::os::OperatingSystemVariant::Ipxe(ipxe) => {
                                let mut tenant_ipxe = ipxe.ipxe_script;
                                let vendor_serial_console = format!(" console={console}");
                                if !tenant_ipxe.contains(&vendor_serial_console) {
                                    let idx = tenant_ipxe.find(" console=");
                                    if let Some(x) = idx {
                                        // insert correct serial console into custom ipxe before any other console=tty* specified
                                        tenant_ipxe.insert_str(x, &vendor_serial_console);
                                    } else {
                                        // this is a strange ipxe script with no console=tty defined, leave it as is
                                    }
                                }
                                tenant_ipxe
                            }
                            model::os::OperatingSystemVariant::OperatingSystemId(os_id) => {
                                let row = db::operating_system::get(txn, os_id).await?;
                                if row.type_
                                    == model::operating_system_definition::OS_TYPE_TEMPLATED_IPXE
                                {
                                    let ipxeos = operating_system_row_to_ipxe_script(&row)?;
                                    Self::render_ipxe_script(&ipxeos, "${base-url}", console)?
                                } else {
                                    row.ipxe_script.unwrap_or_default()
                                }
                            }
                            model::os::OperatingSystemVariant::OsImage(id) => {
                                let os_image = db::os_image::get(txn, id).await?;
                                if os_image.attributes.create_volume {
                                    // this is a block storage os image
                                    // boot will be via the block storage snapshot volume
                                    // no ipxe script for os imaging
                                    exit_instructions(
                                        machine_id,
                                        target.interface_id,
                                        machine.current_state(),
                                    )
                                } else {
                                    let mut qcow_imaging_ipxe = format!(
                                        "{} console={},115200 image_url={} image_sha={}",
                                        qcow_imager_url,
                                        console,
                                        os_image.attributes.source_url,
                                        os_image.attributes.digest
                                    );
                                    if let Some(x) = os_image.attributes.auth_token {
                                        qcow_imaging_ipxe +=
                                            format!(" image_auth_token={x}").as_str();
                                    }
                                    if let Some(x) = os_image.attributes.auth_type {
                                        qcow_imaging_ipxe +=
                                            format!(" image_auth_type={x}").as_str();
                                    }
                                    if let Some(x) = os_image.attributes.rootfs_id {
                                        qcow_imaging_ipxe += format!(" rootfs_uuid={x}").as_str();
                                    }
                                    if let Some(x) = os_image.attributes.rootfs_label {
                                        qcow_imaging_ipxe += format!(" rootfs_label={x}").as_str();
                                    }
                                    if let Some(x) = os_image.attributes.boot_disk {
                                        qcow_imaging_ipxe += format!(" image_disk={x}").as_str();
                                    }
                                    if let Some(x) = os_image.attributes.bootfs_id {
                                        qcow_imaging_ipxe += format!(" bootfs_uuid={x}").as_str();
                                    }
                                    if let Some(x) = os_image.attributes.efifs_id {
                                        qcow_imaging_ipxe += format!(" efifs_uuid={x}").as_str();
                                    }
                                    if instance.config.os.user_data.is_some() {
                                        qcow_imaging_ipxe +=
                                            " ds=nocloud-net;s=${tenant-cloudinit-url}";
                                    }
                                    qcow_imaging_ipxe += "\r\nboot";
                                    qcow_imaging_ipxe
                                }
                            }
                        };

                        if retry_on_failure {
                            // Tell the embedded iPXE menu to retry this one-time script from
                            // memory if it returns an error; the database flag is now consumed.
                            format!("set nico-retry-provisioning 1\n{provisioning_script}")
                        } else {
                            provisioning_script
                        }
                    } else {
                        exit_instructions(machine_id, target.interface_id, machine.current_state())
                    }
                }
                InstanceState::BootingWithDiscoveryImage { .. }
                | InstanceState::HostReprovision { .. } => {
                    PxeInstructions::get_pxe_instruction_for_arch(
                        target.arch,
                        target.interface_id,
                        interface.mac_address,
                        console,
                        machine.id.machine_type(),
                    )
                }

                _ => error_instructions(machine_id, target.interface_id, machine.current_state()),
            },
            ManagedHostState::HostReprovision {
                reprovision_state: HostReprovisionState::WaitingForManualUpgrade { .. },
                ..
            } => PxeInstructions::get_pxe_instruction_for_arch(
                target.arch,
                target.interface_id,
                interface.mac_address,
                console,
                machine.id.machine_type(),
            ),
            x => error_instructions(machine_id, target.interface_id, x),
        };

        Ok(pxe_script)
    }
}

#[cfg(test)]
mod tests {
    use mac_address::MacAddress;

    #[test]
    /// test_formatted_mac_for_instruction_generator makes sure the MAC address
    /// does what we want/expect as part of how we pass it to the instruction
    /// generator.
    fn test_formatted_mac_for_instruction_generator() {
        // also confirm lower -> upper while we're at it
        let mac_address: MacAddress = "aa:bb:cc:dd:ee:ff".parse().unwrap();
        assert_eq!("AA:BB:CC:DD:EE:FF".to_string(), format!("{mac_address}"));
    }
}
