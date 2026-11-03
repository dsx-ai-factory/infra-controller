// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::path::PathBuf;

use nv_redfish_csdl_compiler::compiler::QualifiedName;
use nv_redfish_csdl_compiler::edmx::QualifiedTypeName;
use nv_redfish_csdl_compiler::generator::server_rust::{Builder, Config, Input};
use nv_redfish_schema::{glob_oem_xml, glob_redfish_xml, out_dir, rerun_for};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sources: Vec<_> = glob_redfish_xml()
        .into_iter()
        .chain(
            ["nvidia", "dell", "hpe", "supermicro", "delta"]
                .into_iter()
                .flat_map(glob_oem_xml),
        )
        .collect();
    let config = Config {
        constructor_max_param_number_threshold: 3,
    };
    rerun_for(&sources);
    let root_names = [
        "DeltaEnergySystemsPowerSupply.v1_0_0.PowerSupply",
        "DellAttributes.v1_0_0.DellAttributes",
        "DellJob.v1_5_0.DellJob",
        "HpeiLO.v2_11_0.HpeiLO",
        "HpeServerBootSettings.v2_0_0.HpeServerBootSettings",
        "KCSInterface.v1_1_0.KCSInterface",
        "SysLockdown.v1_0_0.SysLockdown",
        "SmcManagerExtensions.v1_0_0.Manager",
        "NvidiaProcessor.v1_4_0.NvidiaGPU",
        "NvidiaProcessorMetrics.v1_5_0.NvidiaGPUProcessorMetrics",
        "NvidiaMemoryMetrics.v1_2_0.NvidiaGPUMemoryMetrics",
        "NvidiaMemoryMetrics.v1_2_0.RowRemapping",
        "NvidiaMetricReport.v1_0_0.NvidiaMetricReport",
        "NvidiaChassis.v1_4_0.NvidiaCBCChassis",
        "NvidiaComputerSystem.v1_0_0.NvidiaComputerSystem",
        "Assembly.Assembly",
        "Assembly.v1_3_0.AssemblyData",
        "Bios.Bios",
        "BootOption.BootOption",
        "Chassis.Chassis",
        "ComponentIntegrity.ComponentIntegrity",
        "ComputerSystem.ComputerSystem",
        "EthernetInterface.EthernetInterface",
        "Event.Event",
        "Event.v1_6_0.EventRecord",
        "EventDestination.EventDestination",
        "HostInterface.HostInterface",
        "LeakDetection.LeakDetection",
        "LeakDetector.LeakDetector",
        "LogEntry.LogEntry",
        "LogService.LogService",
        "Manager.Manager",
        "ManagerNetworkProtocol.ManagerNetworkProtocol",
        "Memory.Memory",
        "MemoryMetrics.MemoryMetrics",
        "NetworkAdapter.NetworkAdapter",
        "NetworkDeviceFunction.NetworkDeviceFunction",
        "PCIeDevice.PCIeDevice",
        "PowerSubsystem.PowerSubsystem",
        "PowerSupply.PowerSupply",
        "Processor.Processor",
        "ProcessorMetrics.ProcessorMetrics",
        "SecureBoot.SecureBoot",
        "Sensor.Sensor",
        "SerialInterface.SerialInterface",
        "ServiceRoot.ServiceRoot",
        "SoftwareInventory.SoftwareInventory",
        "ThermalSubsystem.ThermalSubsystem",
        "UpdateService.UpdateService",
    ]
    .iter()
    .map(|name| name.parse())
    .collect::<Result<Vec<QualifiedTypeName>, _>>()
    .map_err(|error| error.to_string())?;
    let roots: Vec<_> = root_names.iter().map(QualifiedName::from).collect();
    let paths: Vec<_> = sources.iter().map(PathBuf::from).collect();
    let generated = Builder::new(&roots)
        .config(config)
        .generate(Input::Files(&paths))?;
    std::fs::write(out_dir().join("redfish_server.rs"), generated)?;
    Ok(())
}
