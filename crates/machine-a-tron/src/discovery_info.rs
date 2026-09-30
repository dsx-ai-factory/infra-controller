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

use bmc_mock::infiniband::Guid;
use bmc_mock::mac_address_pool::MacAddressPool;
use bmc_mock::{DpuMachineInfo, HardwareType, HostMachineInfo, MachineInfo};
use carbide_utils::arch::CpuArchitecture;
use mac_address::MacAddress;
use rpc::machine_discovery::{
    BlockDevice, CpuInfo, DiscoveryInfo, DmiData, DpuData, Gpu, GpuPlatformInfo,
    InfinibandInterface, MemoryDeviceGroup, NetworkInterface, NvmeDevice, PciDeviceProperties,
    TpmDescription,
};

pub(crate) fn for_machine(machine: &MachineInfo) -> DiscoveryInfo {
    match machine {
        MachineInfo::Host(host) => for_host(host),
        MachineInfo::Dpu(dpu) => for_dpu(dpu),
    }
}

fn for_dpu(dpu: &DpuMachineInfo) -> DiscoveryInfo {
    match dpu.hw_type {
        HardwareType::DellPowerEdgeR760Bf4 | HardwareType::NvidiaDgxVr => bluefield4(dpu),
        HardwareType::DellPowerEdgeR750
        | HardwareType::WiwynnGB200Nvl
        | HardwareType::LenovoGB300Nvl
        | HardwareType::NvidiaDgxGb300
        | HardwareType::SupermicroGb300Nvl
        | HardwareType::NvidiaDgxH100
        | HardwareType::GenericAmi
        | HardwareType::HpeProliantDl380aGen11
        | HardwareType::GenericSupermicro => bluefield3(dpu),
        HardwareType::LiteOnPowerShelf
        | HardwareType::DeltaPowerShelf
        | HardwareType::NvidiaSwitchNd5200Ld
        | HardwareType::NvidiaSwitchN5700Ld => {
            panic!("DPU discovery is not defined for {}", dpu.hw_type)
        }
    }
}

fn for_host(host: &HostMachineInfo) -> DiscoveryInfo {
    match host.hw_type {
        HardwareType::DellPowerEdgeR750 => dell_poweredge(host, "PowerEdge R750", "1.13.2"),
        HardwareType::DellPowerEdgeR760Bf4 => dell_poweredge(host, "PowerEdge R760", "2.2.7"),
        HardwareType::WiwynnGB200Nvl => wiwynn_gb200(host),
        HardwareType::LenovoGB300Nvl => lenovo_gb300(host),
        HardwareType::NvidiaDgxGb300 => DiscoveryInfo {
            network_interfaces: vec![generic_nic(
                required_dpu(host).host_mac_address,
                0x0006_0003,
                "Mellanox Technologies",
                "BlueField-3 SmartNIC Main Card",
                Some("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
            )],
            ..Default::default()
        },
        HardwareType::NvidiaDgxH100 => nvidia_dgx_h100(host),
        HardwareType::HpeProliantDl380aGen11 => hpe_proliant(host),
        HardwareType::GenericAmi
        | HardwareType::GenericSupermicro
        | HardwareType::SupermicroGb300Nvl
        | HardwareType::NvidiaDgxVr => DiscoveryInfo::default(),
        HardwareType::LiteOnPowerShelf
        | HardwareType::DeltaPowerShelf
        | HardwareType::NvidiaSwitchNd5200Ld
        | HardwareType::NvidiaSwitchN5700Ld => {
            panic!("discovery_info requested for {}", host.hw_type)
        }
    }
}

fn architecture(architecture: CpuArchitecture) -> (String, Option<i32>) {
    (
        architecture.to_string(),
        Some(rpc::utils::cpu_architecture_to_rpc(architecture)),
    )
}

fn bluefield3(dpu: &DpuMachineInfo) -> DiscoveryInfo {
    let part_number = match dpu.hw_type {
        HardwareType::WiwynnGB200Nvl
        | HardwareType::LenovoGB300Nvl
        | HardwareType::NvidiaDgxGb300
        | HardwareType::SupermicroGb300Nvl => "900-9D3B6-00CN-PA0",
        _ if dpu.settings.nic_mode => "900-9D3B4-00CC-EA0",
        _ => "900-9D3B6-00CV-AA0",
    };
    let nic_firmware = dpu
        .settings
        .firmware_versions
        .nic
        .clone()
        .unwrap_or_default();
    let (machine_type, machine_arch) = architecture(CpuArchitecture::Aarch64);

    DiscoveryInfo {
        cpu_info: vec![CpuInfo {
            model: "Cortex-A78AE".into(),
            vendor: "ARM".into(),
            sockets: 1,
            cores: 16,
            threads: 16,
        }],
        block_devices: std::iter::once(BlockDevice {
            model: "KBG40ZPZ128G TOSHIBA MEMORY".into(),
            revision: "AEGA0103".into(),
            serial: "FAKESERNUM0".into(),
            device_type: "disk".into(),
        })
        .chain((0..3).map(|_| BlockDevice {
            model: "NO_MODEL".into(),
            revision: "NO_REVISION".into(),
            serial: "NO_SERIAL".into(),
            device_type: "disk".into(),
        }))
        .collect(),
        machine_type,
        machine_arch,
        dmi_data: Some(DmiData {
            board_name: "Bluefield-3 DPU".into(),
            board_version: "AG".into(),
            bios_version: "4.13.0-26-g337fea6bfd".into(),
            bios_date: "Nov  3 2025".into(),
            product_serial: dpu.serial.clone(),
            board_serial: "Unspecified Base Board Serial Number".into(),
            chassis_serial: "Unspecified Chassis Board Serial Number".into(),
            product_name: "BlueField-3 DPU".into(),
            sys_vendor: "Nvidia".into(),
        }),
        dpu_info: Some(DpuData {
            part_number: part_number.into(),
            part_description: format!("NVIDIA Bluefield-3 {part_number}"),
            product_version: nic_firmware.clone(),
            factory_mac_address: dpu.host_mac_address.to_string(),
            firmware_version: nic_firmware,
            firmware_date: "11.11.2025".into(),
            switches: vec![],
        }),
        ..Default::default()
    }
}

fn bluefield4(dpu: &DpuMachineInfo) -> DiscoveryInfo {
    let part_number = match dpu.hw_type {
        HardwareType::DellPowerEdgeR760Bf4 => "900-9D4B4-CWAA-TSA",
        HardwareType::NvidiaDgxVr => "900-9D4A4-00CB-TS4",
        _ => unreachable!("invalid BF4 platform"),
    };
    let (machine_type, machine_arch) = architecture(CpuArchitecture::Aarch64);

    DiscoveryInfo {
        machine_type,
        machine_arch,
        dmi_data: Some(DmiData {
            board_name: "BlueField-4 DPU".into(),
            product_serial: dpu.serial.clone(),
            board_serial: "Unspecified Base Board Serial Number".into(),
            chassis_serial: "Unspecified Chassis Board Serial Number".into(),
            product_name: "BlueField-4 DPU".into(),
            sys_vendor: "Nvidia".into(),
            ..Default::default()
        }),
        dpu_info: Some(DpuData {
            part_number: part_number.into(),
            part_description: format!("NVIDIA BlueField-4 {part_number}"),
            factory_mac_address: dpu.host_mac_address.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn dell_poweredge(host: &HostMachineInfo, product_name: &str, bios_version: &str) -> DiscoveryInfo {
    let (machine_type, machine_arch) = architecture(CpuArchitecture::X86_64);
    let network_interfaces = if host.hw_type == HardwareType::DellPowerEdgeR760Bf4 {
        vec![generic_nic(
            required_dpu(host).host_mac_address,
            2,
            "Mellanox Technologies",
            "B4240",
            Some("CX9 Family [ConnectX-9]"),
        )]
    } else {
        generic_host_nics(host)
    };

    DiscoveryInfo {
        network_interfaces,
        cpu_info: vec![CpuInfo {
            model: "Intel(R) Xeon(R) Gold 6354 CPU @ 3.00GHz".into(),
            vendor: "GenuineIntel".into(),
            sockets: 2,
            cores: 18,
            threads: 36,
        }],
        block_devices: (0..2)
            .map(|index| BlockDevice {
                model: "Dell Ent NVMe v2 AGN RI U.2 1.92TB".into(),
                revision: "2.3.0".into(),
                serial: format!("FAKESERNUM{index}"),
                device_type: String::new(),
            })
            .collect(),
        machine_type,
        machine_arch,
        dmi_data: Some(DmiData {
            board_name: "01J4WF".into(),
            board_version: "A05".into(),
            bios_version: bios_version.into(),
            bios_date: "12/19/2023".into(),
            product_serial: host.serial.clone(),
            board_serial: format!(".{}.FAKESERNUM2.", host.serial),
            chassis_serial: host.serial.clone(),
            product_name: product_name.into(),
            sys_vendor: "Dell Inc.".into(),
        }),
        memory_device_groups: memory_device_groups(8, 16384, "DDR4"),
        #[allow(deprecated)]
        memory_devices: vec![],
        ..Default::default()
    }
}

fn hpe_proliant(host: &HostMachineInfo) -> DiscoveryInfo {
    let (machine_type, machine_arch) = architecture(CpuArchitecture::X86_64);
    let storage = (0..2).map(|index| {
        let suffix = if index == 0 { 'X' } else { 'W' };
        (format!("KNC8N5359I0108R1{suffix}"), index)
    });
    let serials = storage.map(|(serial, _)| serial).collect::<Vec<_>>();

    DiscoveryInfo {
        network_interfaces: generic_host_nics(host),
        cpu_info: vec![CpuInfo {
            model: "INTEL(R) XEON(R) GOLD 6542Y".into(),
            vendor: "GenuineIntel".into(),
            sockets: 2,
            cores: 24,
            threads: 48,
        }],
        block_devices: serials
            .iter()
            .map(|serial| BlockDevice {
                model: "VO001920KXPTN".into(),
                revision: "HPK0".into(),
                serial: serial.clone(),
                device_type: "disk".into(),
            })
            .collect(),
        machine_type,
        machine_arch,
        nvme_devices: serials
            .into_iter()
            .enumerate()
            .map(|(index, serial)| NvmeDevice {
                model: "VO001920KXPTN".into(),
                firmware_rev: "HPK0".into(),
                serial,
                size_mb: Some(nvme_size_mb("VO001920KXPTN")),
                pci_path: Some(nvme_pci_path(index)),
            })
            .collect(),
        dmi_data: Some(DmiData {
            board_name: "ProLiant DL380a Gen11".into(),
            bios_version: "2.22".into(),
            bios_date: "06/19/2024".into(),
            product_serial: host.serial.clone(),
            board_serial: "PYFCA0ARHJM00Z".into(),
            chassis_serial: host.serial.clone(),
            product_name: "ProLiant DL380a Gen11".into(),
            sys_vendor: "HPE".into(),
            ..Default::default()
        }),
        memory_device_groups: memory_device_groups(16, 16384, "DDR5"),
        #[allow(deprecated)]
        memory_devices: vec![],
        ..Default::default()
    }
}

fn wiwynn_gb200(host: &HostMachineInfo) -> DiscoveryInfo {
    let (machine_type, machine_arch) = architecture(CpuArchitecture::Aarch64);
    let dpus = required_dpus::<2>(host);

    DiscoveryInfo {
        network_interfaces: vec![
            generic_nic(
                dpus[0].host_mac_address,
                0x0016_0003,
                "Mellanox Technologies",
                "BlueField-3 SmartNIC Main Card",
                Some("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
            ),
            generic_nic(
                dpus[1].host_mac_address,
                0x0006_0003,
                "Mellanox Technologies",
                "BlueField-3 SmartNIC Main Card",
                Some("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
            ),
        ],
        infiniband_interfaces: gb200_infiniband_interfaces(host),
        cpu_info: vec![CpuInfo {
            model: "Neoverse-V2".into(),
            vendor: "ARM".into(),
            sockets: 2,
            cores: 72,
            threads: 72,
        }],
        block_devices: (0..9)
            .map(|index| BlockDevice {
                model: "SAMSUNG MZTL63T8HFLT-00AW7".into(),
                revision: "LDDL4U2Q".into(),
                serial: format!("BDFAKESERNUM{index}"),
                device_type: "disk".into(),
            })
            .collect(),
        machine_type,
        machine_arch,
        nvme_devices: (0..9)
            .map(|index| NvmeDevice {
                model: "SAMSUNG MZTL63T8HFLT-00AW7".into(),
                firmware_rev: "LDDL4U2Q".into(),
                serial: format!("BDFAKESERNUM{index}"),
                size_mb: Some(nvme_size_mb("SAMSUNG MZTL63T8HFLT-00AW7")),
                pci_path: Some(nvme_pci_path(index)),
            })
            .collect(),
        dmi_data: Some(DmiData {
            board_name: "KINABALU BMC CARD".into(),
            board_version: "PVT".into(),
            bios_version: "00000083".into(),
            bios_date: "20260107".into(),
            product_serial: host.serial.clone(),
            board_serial: host.serial.clone(),
            chassis_serial: host.serial.clone(),
            product_name: "GB200 NVL".into(),
            sys_vendor: "NVIDIA".into(),
        }),
        gpus: gb200_gpus(),
        memory_device_groups: memory_device_groups(2, 491520, "LPDDR5"),
        #[allow(deprecated)]
        memory_devices: vec![],
        ..Default::default()
    }
}

fn lenovo_gb300(host: &HostMachineInfo) -> DiscoveryInfo {
    let (machine_type, machine_arch) = architecture(CpuArchitecture::Aarch64);
    let storage = [
        ("SAMSUNG MZTL63T8HFLT-00AW7", "LDDL4U2Q", "LENOVOGB300NVME0"),
        ("SAMSUNG MZTL63T8HFLT-00AW7", "LDDL4U2Q", "LENOVOGB300NVME1"),
        ("SAMSUNG MZTL63T8HFLT-00AW7", "LDDL4U2Q", "LENOVOGB300NVME2"),
        ("SAMSUNG MZTL63T8HFLT-00AW7", "LDDL4U2Q", "LENOVOGB300NVME3"),
        ("SAMSUNG MZ1L21T9HCLS-00A07", "GDC7802Q", "LENOVOGB300NVME4"),
    ];

    DiscoveryInfo {
        network_interfaces: lenovo_network_interfaces(host),
        cpu_info: vec![CpuInfo {
            model: "Neoverse-V2".into(),
            vendor: "ARM".into(),
            sockets: 2,
            cores: 72,
            threads: 72,
        }],
        block_devices: storage
            .iter()
            .map(|(model, revision, serial)| BlockDevice {
                model: (*model).into(),
                revision: (*revision).into(),
                serial: (*serial).into(),
                device_type: "disk".into(),
            })
            .collect(),
        machine_type,
        machine_arch,
        nvme_devices: storage
            .iter()
            .enumerate()
            .map(|(index, (model, firmware_rev, serial))| NvmeDevice {
                model: (*model).into(),
                firmware_rev: (*firmware_rev).into(),
                serial: (*serial).into(),
                size_mb: Some(nvme_size_mb(model)),
                pci_path: Some(nvme_pci_path(index)),
            })
            .collect(),
        dmi_data: Some(DmiData {
            board_name: "PG548".into(),
            board_version: "699-2G548-0301-B00".into(),
            bios_version: "GBHC01A_01.05.0".into(),
            product_serial: host.serial.clone(),
            board_serial: "165300000001".into(),
            chassis_serial: host.serial.clone(),
            bios_date: "03/05/2026".into(),
            product_name: "HG635N_V2".into(),
            sys_vendor: "Lenovo".into(),
        }),
        gpus: (0..4)
            .map(|index| Gpu {
                name: "NVIDIA GB300".into(),
                serial: [
                    "165300000001",
                    "165300000001",
                    "165300000002",
                    "165300000002",
                ][index]
                    .into(),
                driver_version: "580.126.16".into(),
                vbios_version: "97.10.4A.00.1A".into(),
                inforom_version: "G548.0301.00.03".into(),
                total_memory: "284208 MiB".into(),
                frequency: "2070 MHz".into(),
                pci_bus_id: [
                    "00000008:06:00.0",
                    "00000009:06:00.0",
                    "00000018:06:00.0",
                    "00000019:06:00.0",
                ][index]
                    .into(),
                platform_info: Some(GpuPlatformInfo {
                    chassis_serial: host.serial.clone(),
                    slot_number: 4,
                    tray_index: 3,
                    host_id: 1,
                    module_id: [2, 1, 4, 3][index],
                    fabric_guid: format!("0xfeeeeeeeeeeeee{index:02x}"),
                }),
            })
            .collect(),
        memory_device_groups: memory_device_groups(2, 491520, "LPDDR5"),
        #[allow(deprecated)]
        memory_devices: vec![],
        tpm_description: Some(TpmDescription {
            vendor: "Could not convert spec_version672".into(),
            firmware_version: "0xf0018.0x4a0a00".into(),
            tpm_spec: "2.0".into(),
        }),
        ..Default::default()
    }
}

fn nvidia_dgx_h100(host: &HostMachineInfo) -> DiscoveryInfo {
    let (machine_type, machine_arch) = architecture(CpuArchitecture::X86_64);
    let mut pool = MacAddressPool::new_pool(host.hw_mac_addr_pool);
    let mut next_mac = || pool.allocate().expect("MAC address must be allocated");

    let _storage_nic_serial_source = next_mac();
    for _ in 0..8 {
        let _ = next_mac();
    }
    let management_mac = next_mac();
    let storage_macs = [next_mac(), next_mac()];

    DiscoveryInfo {
        network_interfaces: vec![
            network_interface(
                management_mac,
                "Intel Corporation",
                "Ethernet Controller X550",
                "/devices/pci0000:00/0000:00:10.0/0000:0b:00.0/net/eno3",
                "0000:0b:00.0",
                0,
            ),
            network_interface(
                storage_macs[0],
                "Mellanox Technologies",
                "MT2910 Family [ConnectX-7]",
                "/devices/pci0000:24/0000:24:01.0/0000:25:00.0/0000:26:00.0/0000:27:00.0/0000:28:00.0/0000:29:00.0/net/enp41s0f0np0",
                "0000:29:00.0",
                0,
            ),
            network_interface(
                storage_macs[1],
                "Mellanox Technologies",
                "MT2910 Family [ConnectX-7]",
                "/devices/pci0000:24/0000:24:01.0/0000:25:00.0/0000:26:00.0/0000:27:00.0/0000:28:00.0/0000:29:00.1/net/enp41s0f1np1",
                "0000:29:00.1",
                0,
            ),
            network_interface(
                required_dpu(host).host_mac_address,
                "Mellanox Technologies",
                "MT43244 BlueField-3 integrated ConnectX-7 network controller",
                "/devices/pci0000:80/0000:80:05.0/0000:82:00.0/net/ens6np0",
                "0000:82:00.0",
                0,
            ),
        ],
        infiniband_interfaces: dgx_h100_infiniband_interfaces(host),
        cpu_info: vec![CpuInfo {
            model: "Intel(R) Xeon(R) Platinum 8480CL".into(),
            vendor: "GenuineIntel".into(),
            sockets: 2,
            cores: 56,
            threads: 112,
        }],
        block_devices: (0..2)
            .map(|index| BlockDevice {
                model: "Micron_7450_MTFDKBG1T9TFR".into(),
                revision: "E2MU200".into(),
                serial: format!("MicronFAKESERNUM{index}"),
                device_type: "disk".into(),
            })
            .chain((0..8).map(|index| BlockDevice {
                model: "KCM6DRUL3T84".into(),
                revision: "0107".into(),
                serial: format!("KCMFAKESERNUM{index}"),
                device_type: "disk".into(),
            }))
            .collect(),
        machine_type,
        machine_arch,
        nvme_devices: (0..2)
            .map(|index| NvmeDevice {
                model: "Micron_7450_MTFDKBG1T9TFR".into(),
                firmware_rev: "E2MU200".into(),
                serial: format!("MicronFAKESERNUM{index}"),
                size_mb: Some(nvme_size_mb("Micron_7450_MTFDKBG1T9TFR")),
                pci_path: Some(nvme_pci_path(index)),
            })
            .chain((0..8).map(|index| NvmeDevice {
                model: "KCM6DRUL3T84".into(),
                firmware_rev: "0107".into(),
                serial: format!("KCMFAKESERNUM{index}"),
                size_mb: Some(nvme_size_mb("KCM6DRUL3T84")),
                pci_path: Some(nvme_pci_path(index + 2)),
            }))
            .collect(),
        dmi_data: Some(DmiData {
            board_name: "DGXH100".into(),
            board_version: "555.07L01.0001".into(),
            bios_version: "1.6.7".into(),
            bios_date: "02/20/2025".into(),
            product_serial: host.serial.clone(),
            board_serial: format!("{}.FAKESERNUM1", host.serial),
            chassis_serial: "1663223000002".into(),
            product_name: "DGXH100".into(),
            sys_vendor: "NVIDIA".into(),
        }),
        gpus: (0..8)
            .map(|index| Gpu {
                name: "NVIDIA H100 80GB HBM3".into(),
                serial: format!("165290000000{}", index + 1),
                driver_version: "580.126.16".into(),
                vbios_version: "96.00.A5.00.01".into(),
                inforom_version: "G520.0200.00.05".into(),
                total_memory: "81559 MiB".into(),
                frequency: "1980 MHz".into(),
                pci_bus_id: [
                    "00000000:1B:00.0",
                    "00000000:43:00.0",
                    "00000000:52:00.0",
                    "00000000:61:00.0",
                    "00000000:9D:00.0",
                    "00000000:C3:00.0",
                    "00000000:D1:00.0",
                    "00000000:DF:00.0",
                ][index]
                    .into(),
                platform_info: None,
            })
            .collect(),
        memory_device_groups: memory_device_groups(32, 65536, "DDR5"),
        #[allow(deprecated)]
        memory_devices: vec![],
        ..Default::default()
    }
}

fn required_dpu(host: &HostMachineInfo) -> &DpuMachineInfo {
    host.dpus.first().expect("DPU must be present")
}

fn required_dpus<const N: usize>(host: &HostMachineInfo) -> [&DpuMachineInfo; N] {
    host.dpus
        .iter()
        .collect::<Vec<_>>()
        .try_into()
        .unwrap_or_else(|_| panic!("{} DPUs must be present", N))
}

fn generic_host_nics(host: &HostMachineInfo) -> Vec<NetworkInterface> {
    if host.dpus.is_empty() {
        host.non_dpu_mac_address
            .iter()
            .enumerate()
            .map(|(index, mac_address)| {
                generic_nic(
                    *mac_address,
                    index + 1,
                    "Rooftop Technologies",
                    "Rooftop 10 Kilobit Ethernet Adapter",
                    None,
                )
            })
            .collect()
    } else {
        host.dpus
            .iter()
            .enumerate()
            .map(|(index, dpu)| {
                generic_nic(
                    dpu.host_mac_address,
                    index + 1,
                    "Mellanox Technologies",
                    "BlueField-3 SmartNIC Main Card",
                    Some("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
                )
            })
            .collect()
    }
}

fn generic_nic(
    mac_address: MacAddress,
    slot: usize,
    vendor: &str,
    device: &str,
    description: Option<&str>,
) -> NetworkInterface {
    let domain = slot >> 16;
    let bus = slot & 0xff;
    let device_name = format!("enp{domain}s{bus}np0");
    let slot = format!("{domain:04x}:{bus:02x}:00.0");
    NetworkInterface {
        mac_address: mac_address.to_string(),
        pci_properties: Some(PciDeviceProperties {
            vendor: vendor.into(),
            device: device.into(),
            path: format!(
                "/devices/pci{domain:04x}:00/{domain:04x}:00:00.0/{slot}/net/{device_name}"
            ),
            numa_node: 0,
            description: description.map(Into::into),
            slot: Some(slot),
        }),
    }
}

fn network_interface(
    mac_address: MacAddress,
    vendor: &str,
    device: &str,
    path: &str,
    slot: &str,
    numa_node: i32,
) -> NetworkInterface {
    NetworkInterface {
        mac_address: mac_address.to_string(),
        pci_properties: Some(PciDeviceProperties {
            vendor: vendor.into(),
            device: device.into(),
            path: path.into(),
            numa_node,
            description: Some(device.into()),
            slot: Some(slot.into()),
        }),
    }
}

/// Builds a single-group mock memory device list for use in simulated machine profiles.
fn memory_device_groups(count: u32, size_mb: u32, memory_type: &str) -> Vec<MemoryDeviceGroup> {
    vec![MemoryDeviceGroup {
        size_mb: Some(size_mb),
        mem_type: Some(memory_type.into()),
        count,
    }]
}

/// Mock NVMe capacity in MiB (matching what the real host enumeration reports),
/// inferred from the capacity encoded in the model string (e.g. `…3T8…` = 3.84
/// TB, otherwise 1.92 TB). Mock hosts must report a size so v5 SKU generation,
/// which rejects drives with unknown size, can run against them.
fn nvme_size_mb(model: &str) -> u32 {
    if model.contains("3T8") {
        3_662_840
    } else {
        1_831_420
    }
}

/// Distinct fake sysfs DEVPATH for the Nth mock NVMe controller, mirroring the
/// shape the real host enumeration reports (`…/<pci function>/nvme/nvmeN`).
/// Each drive sits behind its own PCI function so v5+ SKU generation records
/// one storage entry per drive.
fn nvme_pci_path(index: usize) -> String {
    format!(
        "/devices/pci0000:00/0000:64:00.0/0000:{:02x}:00.0/nvme/nvme{index}",
        0x65 + index
    )
}

fn gb200_gpus() -> Vec<Gpu> {
    (0..2)
        .flat_map(|board| {
            (0..2).map(move |gpu| Gpu {
                name: "NVIDIA GB200".into(),
                serial: format!("16530000000{}", board + 1),
                driver_version: "580.126.16".into(),
                vbios_version: "97.00.B9.00.76".into(),
                inforom_version: "G548.0201.00.06".into(),
                total_memory: "189471 MiB".into(),
                frequency: "2062 MHz".into(),
                pci_bus_id: [
                    ["00000008:01:00.0", "00000009:01:00.0"],
                    ["00000018:01:00.0", "00000019:01:00.0"],
                ][board][gpu]
                    .into(),
                platform_info: Some(GpuPlatformInfo {
                    chassis_serial: format!("182100000000{board}{gpu}"),
                    slot_number: 24,
                    tray_index: 14,
                    host_id: 1,
                    module_id: [[2, 1], [4, 3]][board][gpu],
                    fabric_guid: format!("0xfeeeeeeeeeeeee{gpu:02x}"),
                }),
            })
        })
        .collect()
}

fn gb200_infiniband_interfaces(host: &HostMachineInfo) -> Vec<InfinibandInterface> {
    let guids: [Guid; 4] = host
        .infiniband_port_guids()
        .try_into()
        .expect("GB200 has four InfiniBand interfaces");
    [(0x0000, 0), (0x0002, 0), (0x0010, 1), (0x0012, 1)]
        .into_iter()
        .zip(guids)
        .map(|((domain, numa_node), guid)| {
            let device_name = if domain == 0 {
                "ibp3s0".to_string()
            } else {
                format!("ibP{domain}p3s0")
            };
            InfinibandInterface {
                pci_properties: Some(PciDeviceProperties {
                    vendor: "Mellanox Technologies".into(),
                    device: "MT2910 Family [ConnectX-7]".into(),
                    path: format!(
                        "/devices/pci{domain:02x}:00/{domain:02x}:00:00.0/{domain:02x}:01:00.0/{domain:02x}:02:00.0/{domain:02x}:03:00.0/infiniband/{device_name}"
                    ),
                    numa_node,
                    description: Some("MT2910 Family [ConnectX-7]".into()),
                    slot: Some(format!("{domain}:03:00.0")),
                }),
                guid: guid.to_string(),
            }
        })
        .collect()
}

fn lenovo_network_interfaces(host: &HostMachineInfo) -> Vec<NetworkInterface> {
    let mut pool = MacAddressPool::new_pool(host.hw_mac_addr_pool);
    let cx8_mac_addresses: [MacAddress; 10] =
        std::array::from_fn(|_| pool.allocate().expect("MAC address must be allocated"));
    let embedded_mac = pool.allocate().expect("MAC address must be allocated");
    let cx8_interfaces = [
        (0x0000, 0, 0),
        (0x0000, 1, 0),
        (0x0000, 2, 0),
        (0x0000, 3, 0),
        (0x0002, 0, 0),
        (0x0002, 1, 0),
        (0x0010, 0, 1),
        (0x0010, 1, 1),
        (0x0012, 0, 1),
        (0x0012, 1, 1),
    ];

    cx8_mac_addresses[..6]
        .iter()
        .zip(&cx8_interfaces[..6])
        .map(|(mac_address, &(domain, function, numa_node))| {
            cx8_network_interface(*mac_address, domain, function, numa_node)
        })
        .chain(std::iter::once(network_interface(
            embedded_mac,
            "Intel Corporation",
            "I210 Gigabit Network Connection",
            "/devices/pci0005:00/0005:00:00.0/0005:01:00.0/0005:02:06.0/0005:09:00.0/net/enP5p9s0",
            "0005:09:00.0",
            0,
        )))
        .chain(cx8_mac_addresses[6..].iter().zip(&cx8_interfaces[6..]).map(
            |(mac_address, &(domain, function, numa_node))| {
                cx8_network_interface(*mac_address, domain, function, numa_node)
            },
        ))
        .chain(std::iter::once(network_interface(
            required_dpu(host).host_mac_address,
            "Mellanox Technologies",
            "MT43244 BlueField-3 integrated ConnectX-7 network controller",
            "/devices/pci0016:00/0016:00:00.0/0016:01:00.0/net/enP22s22np0",
            "0016:01:00.0",
            1,
        )))
        .collect()
}

fn cx8_network_interface(
    mac_address: MacAddress,
    domain: u16,
    function: u8,
    numa_node: i32,
) -> NetworkInterface {
    let device_name = if domain == 0 {
        format!("enp3s0f{function}np{function}")
    } else {
        format!("enP{domain}p3s0f{function}np{function}")
    };
    let path = format!(
        "/devices/pci{domain:04x}:00/{domain:04x}:00:00.0/{domain:04x}:01:00.0/{domain:04x}:02:00.0/{domain:04x}:03:00.{function}/net/{device_name}"
    );
    let slot = format!("{domain:04x}:03:00.{function}");
    network_interface(
        mac_address,
        "Mellanox Technologies",
        "CX8 Family [ConnectX-8]",
        &path,
        &slot,
        numa_node,
    )
}

fn dgx_h100_infiniband_interfaces(host: &HostMachineInfo) -> Vec<InfinibandInterface> {
    let guids: [Guid; 8] = host
        .infiniband_port_guids()
        .try_into()
        .expect("DGX H100 has eight InfiniBand interfaces");
    [
        (0x15, 0),
        (0x3d, 0),
        (0x4c, 0),
        (0x5b, 0),
        (0x97, 1),
        (0xbd, 1),
        (0xcb, 1),
        (0xd9, 1),
    ]
    .into_iter()
    .zip(guids)
    .map(|((bus, numa_node), guid)| {
        let device_name = format!("ibp{bus}s0");
        InfinibandInterface {
            pci_properties: Some(PciDeviceProperties {
                vendor: "Mellanox Technologies".into(),
                device: "MT2910 Family [ConnectX-7]".into(),
                path: format!(
                    "/devices/pci0000:{bus:02x}/0000:{bus:02x}:01.0/0000:{:02x}:00.0/0000:{:02x}:00.0/0000:{:02x}:00.0/infiniband/{device_name}",
                    bus + 1,
                    bus + 2,
                    bus + 3,
                ),
                numa_node,
                description: Some("MT2910 Family [ConnectX-7]".into()),
                slot: Some(format!("0000:{:02x}:00.0", bus + 3)),
            }),
            guid: guid.to_string(),
        }
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use bmc_mock::mac_address_pool::{Config, MacAddressPool, PoolConfig};
    use bmc_mock::{DpuMachineInfo, DpuSettings};

    use super::*;

    /// Builds a mock host with the platform's fixed number of DPUs.
    fn host_for_platform(platform: HardwareType) -> MachineInfo {
        let pool_config =
            PoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 16).expect("valid MAC pool");
        let hardware_pool_config =
            PoolConfig::new(MacAddress::new([6, 0, 0, 0, 0, 0]), 16).expect("valid MAC pool");
        let mut pool = MacAddressPool::new(Config {
            ranges: None,
            pool: Some(pool_config),
        });
        let dpu_count = platform.fixed_number_of_dpu().unwrap_or(1);
        let dpus = (0..dpu_count)
            .map(|_| DpuMachineInfo::new(platform, &mut pool, DpuSettings::default()))
            .collect();
        MachineInfo::Host(HostMachineInfo::new(
            platform,
            dpus,
            &mut pool,
            hardware_pool_config,
        ))
    }

    fn lenovo_host() -> MachineInfo {
        host_for_platform(HardwareType::LenovoGB300Nvl)
    }

    #[test]
    fn lenovo_gb300_discovery_matches_platform_shape() {
        let machine = lenovo_host();
        let host = match &machine {
            MachineInfo::Host(host) => host,
            MachineInfo::Dpu(_) => unreachable!("Lenovo GB300 must be a host"),
        };
        let discovery = for_machine(&machine);

        assert_eq!(discovery.network_interfaces.len(), 12);
        assert_eq!(
            discovery
                .network_interfaces
                .iter()
                .map(|interface| interface.mac_address.as_str())
                .collect::<HashSet<_>>()
                .len(),
            12
        );
        let pci_properties = discovery
            .network_interfaces
            .iter()
            .map(|interface| interface.pci_properties.as_ref().expect("PCI properties"))
            .collect::<Vec<_>>();
        assert_eq!(
            pci_properties
                .iter()
                .map(|pci| pci.slot.as_deref().expect("PCI slot"))
                .collect::<Vec<_>>(),
            [
                "0000:03:00.0",
                "0000:03:00.1",
                "0000:03:00.2",
                "0000:03:00.3",
                "0002:03:00.0",
                "0002:03:00.1",
                "0005:09:00.0",
                "0010:03:00.0",
                "0010:03:00.1",
                "0012:03:00.0",
                "0012:03:00.1",
                "0016:01:00.0",
            ]
        );
        assert_eq!(
            pci_properties
                .iter()
                .map(|pci| pci.numa_node)
                .collect::<Vec<_>>(),
            [0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1]
        );
        assert_eq!(discovery.cpu_info[0].model, "Neoverse-V2");
        assert_eq!(discovery.machine_type, "aarch64");
        assert_eq!(discovery.block_devices.len(), 5);
        assert_eq!(discovery.nvme_devices.len(), 5);
        for (block, nvme) in discovery.block_devices.iter().zip(&discovery.nvme_devices) {
            assert_eq!(block.model, nvme.model);
            assert_eq!(block.revision, nvme.firmware_rev);
            assert_eq!(block.serial, nvme.serial);
        }
        let dmi = discovery.dmi_data.as_ref().expect("DMI data");
        assert_eq!(dmi.product_serial, host.serial);
        assert_eq!(dmi.chassis_serial, host.serial);
        assert_eq!(dmi.board_serial, "165300000001");
        assert_eq!(discovery.gpus.len(), 4);
        assert_eq!(
            discovery
                .gpus
                .iter()
                .map(|gpu| {
                    gpu.platform_info
                        .as_ref()
                        .expect("GPU platform info")
                        .module_id
                })
                .collect::<Vec<_>>(),
            [2, 1, 4, 3]
        );
        assert_eq!(
            discovery.memory_device_groups,
            vec![MemoryDeviceGroup {
                size_mb: Some(491520),
                mem_type: Some("LPDDR5".into()),
                count: 2,
            }]
        );
        assert!(discovery.tpm_description.is_some());
    }

    #[test]
    fn discovery_is_defined_for_machine_platforms() {
        let platforms = [
            HardwareType::DellPowerEdgeR750,
            HardwareType::DellPowerEdgeR760Bf4,
            HardwareType::WiwynnGB200Nvl,
            HardwareType::LenovoGB300Nvl,
            HardwareType::NvidiaDgxGb300,
            HardwareType::SupermicroGb300Nvl,
            HardwareType::NvidiaDgxVr,
            HardwareType::NvidiaDgxH100,
            HardwareType::GenericAmi,
            HardwareType::HpeProliantDl380aGen11,
            HardwareType::GenericSupermicro,
        ];

        for platform in platforms {
            for_machine(&host_for_platform(platform));
        }
    }

    #[test]
    fn wiwynn_gb200_reports_distinct_pci_locations() {
        let discovery = for_machine(&host_for_platform(HardwareType::WiwynnGB200Nvl));

        assert_eq!(
            discovery
                .network_interfaces
                .iter()
                .filter_map(|interface| interface.pci_properties.as_ref())
                .map(|properties| {
                    (
                        properties.slot.as_deref().expect("PCI slot"),
                        properties.path.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            [
                (
                    "0016:03:00.0",
                    "/devices/pci0016:00/0016:00:00.0/0016:03:00.0/net/enp22s3np0",
                ),
                (
                    "0006:03:00.0",
                    "/devices/pci0006:00/0006:00:00.0/0006:03:00.0/net/enp6s3np0",
                ),
            ],
            "Wiwynn DPUs must report their distinct expected PCI locations",
        );
    }

    #[test]
    fn discovery_is_defined_for_bf3_and_bf4_dpus() {
        for platform in [
            HardwareType::DellPowerEdgeR750,
            HardwareType::DellPowerEdgeR760Bf4,
        ] {
            let pool_config =
                PoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 16).expect("valid pool");
            let mut pool = MacAddressPool::new(Config {
                ranges: None,
                pool: Some(pool_config),
            });
            let dpu = DpuMachineInfo::new(platform, &mut pool, DpuSettings::default());
            let discovery = for_machine(&MachineInfo::Dpu(dpu));

            assert_eq!(discovery.machine_type, "aarch64");
            assert!(discovery.dpu_info.is_some());
        }
    }

    #[test]
    fn lenovo_gb300_discovery_includes_dpu_host_interface() {
        let machine = lenovo_host();
        let expected_mac = match &machine {
            MachineInfo::Host(host) => required_dpu(host).host_mac_address.to_string(),
            MachineInfo::Dpu(_) => unreachable!("Lenovo GB300 must be a host"),
        };
        let discovery = for_machine(&machine);

        let dpu_interface = discovery
            .network_interfaces
            .iter()
            .find(|interface| interface.mac_address == expected_mac)
            .expect("discovery must include the DPU host interface");
        let pci = dpu_interface
            .pci_properties
            .as_ref()
            .expect("DPU host interface must include PCI properties");
        assert!(pci.vendor.to_ascii_lowercase().contains("mellanox"));
        assert_eq!(pci.slot.as_deref(), Some("0016:01:00.0"));
    }
}
