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

//! The NVOS of an NVLink switch tray, as its NVUE REST API shows it.
//!
//! Both NVLink switch tray profiles run the same NVOS and carry the same
//! sensors, so they share this description and differ in system type, part
//! number and serial number. The description follows a capture taken on
//! 2026-10-13 from an N5400_LD switch tray running NVOS 25.02.4463: the
//! sensor roster, names and thresholds, the fan speeds and the application
//! versions are that tray's, while the NVLink port count, the partition and
//! the management address are the simulated switch's.

use mac_address::MacAddress;

use crate::nvue;

/// The NVOS release the trays run.
const PRODUCT_RELEASE: &str = "25.02.4463";

/// Fan trays and fans per tray.
const FAN_TRAYS: u32 = 4;
const FANS_PER_TRAY: u32 = 2;

/// Leak sensors, `LEAKAGE-1` to `LEAKAGE-<n>`.
const LEAK_SENSORS: u32 = 6;

/// NVLink ports, `acp1` to `acp<n>`.
const NVL_PORTS: u32 = 144;

/// The factory partition every fabric manager starts with, holding every GPU
/// of an NVL72 rack.
const FACTORY_PARTITION_ID: &str = "32766";
const FACTORY_PARTITION_GPUS: u32 = 72;

/// The NVOS of an NVLink switch tray of `system_type` with `part_number` and
/// `switch_serial_number`, whose management interface has `management_mac`,
/// administered by `admin`.
pub(super) fn nvl_switch_tray_config(
    system_type: &'static str,
    part_number: &'static str,
    switch_serial_number: &str,
    management_mac: Option<MacAddress>,
    admin: nvue::Account,
) -> nvue::Config {
    nvue::Config {
        hostname: format!("nvos-{}", switch_serial_number.to_ascii_lowercase()),
        system_type,
        part_number,
        product_release: PRODUCT_RELEASE,
        serial_number: switch_serial_number.to_string(),
        management_mac,
        admin,
        fans: (1..=FAN_TRAYS)
            .flat_map(|tray| {
                (1..=FANS_PER_TRAY).map(move |fan| nvue::FanConfig {
                    name: format!("FAN{tray}/{fan}"),
                    direction: "F2B",
                    max_speed_rpm: 37950,
                    min_speed_rpm: 6000,
                    // The first fan of a tray runs a little faster than the second.
                    current_speed_rpm: if fan == 1 { 10096 } else { 9014 },
                })
            })
            .collect(),
        temperatures: temperatures(),
        leak_sensors: (1..=LEAK_SENSORS)
            .map(|sensor| format!("LEAKAGE-{sensor}"))
            .collect(),
        nvl_ports: NVL_PORTS,
        cluster_apps: vec![
            nvue::ClusterAppConfig {
                name: "nmx-controller",
                app_id: "nmx-c-nvos",
                app_version: "4.21.156",
                components_version: "sm:2025.10.18, gfm:580.173.04, fib:4.21.156",
                capabilities: "sm, gfm, fib, gw-api",
                addition_info: "CONTROL_PLANE_STATE_CONFIGURED",
            },
            nvue::ClusterAppConfig {
                name: "nmx-telemetry",
                app_id: "nmx-telemetry",
                app_version: "4.20.9",
                components_version: "nvl:1.23.7, aggr:1.30.1, cm:1.1.14, gnmi:1.4.1",
                capabilities: "nvl, gnmi, syslog, bmc",
                addition_info: "",
            },
        ],
        partitions: vec![nvue::PartitionConfig {
            id: FACTORY_PARTITION_ID.to_string(),
            name: "Default Partition".to_string(),
            num_gpus: FACTORY_PARTITION_GPUS,
            partition_type: "gpuuid_based",
            resiliency_mode: "adaptive_bandwidth",
        }],
    }
}

/// The tray's temperature sensors, in degrees Celsius.
fn temperatures() -> Vec<nvue::TemperatureConfig> {
    vec![
        sensor("ASIC1", 34.0, None, Some(105.0), Some(120.0)),
        sensor("ASIC2", 34.0, None, Some(105.0), Some(120.0)),
        sensor("Ambient-MNG-Temp", 23.5, None, None, None),
        sensor("CPU-Pack-Temp", 43.75, None, Some(95.0), Some(100.0)),
        sensor("Drive-Temp", 44.85, Some(-0.15), Some(99.85), Some(109.85)),
        sensor("HSC-VinDC-Temp", 26.69, None, Some(255.94), Some(255.94)),
        sensor("PDB-Conv-1-Temp", 28.0, None, None, Some(110.0)),
        sensor("PDB-Conv-2-Temp", 29.0, None, None, Some(110.0)),
        sensor("PMIC-1-Temp", 32.0, None, Some(105.0), Some(125.0)),
        sensor("PMIC-2-Temp", 31.0, None, Some(105.0), Some(125.0)),
        sensor("PMIC-3-Temp", 35.0, None, Some(105.0), Some(125.0)),
        sensor("PMIC-4-Temp", 32.0, None, Some(105.0), Some(125.0)),
        sensor("PMIC-5-Temp", 35.0, None, Some(105.0), Some(125.0)),
        sensor("PMIC-6-Temp", 34.0, None, Some(105.0), Some(125.0)),
        sensor("PMIC-7-Temp", 34.0, None, None, Some(120.0)),
        sensor("PMIC-8-Temp", 30.0, None, Some(105.0), Some(115.0)),
        sensor("SODIMM-1-Temp", 26.0, None, Some(85.0), Some(95.0)),
        sensor("SWB-ASIC1-PCB-Temp", 27.62, None, None, None),
        sensor("SWB-ASIC2-PCB-Temp", 27.69, None, None, None),
    ]
}

/// A temperature sensor `name` reading `current`, with the `min`, `max` and
/// `crit` thresholds it has.
fn sensor(
    name: &str,
    current: f32,
    min: Option<f32>,
    max: Option<f32>,
    crit: Option<f32>,
) -> nvue::TemperatureConfig {
    nvue::TemperatureConfig {
        name: name.to_string(),
        current,
        min,
        max,
        crit,
    }
}
