/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use Capability::{
    Accounts, Attestation, Bios, BmcControl, BootOrder, Console, Dpu, Firmware, Lockdown, Power,
    SecureBoot, SuperNic,
};
use Quirk::{
    BlueFieldNicModeBiosError, BlueFieldNicModeUnreadable, BlueFieldOemTimeoutInNicMode,
    BlueFieldSpacedBiosAttributeNames, DellUefiPasswordClearJobFails, LenovoForceRestartHangs,
    RedfishRestartCutsDpuPower, SupermicroHostInterfaceRequired, SupermicroIpmiHostInterface,
    SupermicroMgxC2, VikingLockdownFirmware,
};
use bmc_platform::{
    ChassisIdentity, FirmwareInventoryIdentity, ManagerIdentity, PlatformIdentity,
    ServiceRootIdentity, SystemIdentity,
};

use super::*;
use crate::drivers::{Driver, Drivers};
use crate::selection::CapabilitySelection::{self, Standard, Unsupported};
use crate::selection::ResolvedSelection;

fn identity(vendor: &str, product: Option<&str>) -> PlatformIdentity {
    PlatformIdentity {
        service_root: ServiceRootIdentity {
            vendor: Some(vendor.to_string()),
            product: product.map(str::to_string),
            oem_keys: Vec::new(),
        },
        manager: Some(ManagerIdentity {
            id: "BMC".to_string(),
            ..ManagerIdentity::default()
        }),
        system: Some(SystemIdentity {
            id: "System".to_string(),
            ..SystemIdentity::default()
        }),
        ..PlatformIdentity::default()
    }
}

fn with_system(mut platform: PlatformIdentity, system: SystemIdentity) -> PlatformIdentity {
    platform.system = Some(system);
    platform
}

fn with_chassis(mut platform: PlatformIdentity, chassis: ChassisIdentity) -> PlatformIdentity {
    platform.chassis = vec![chassis];
    platform
}

fn lenovo_ami() -> PlatformIdentity {
    let mut platform = identity("Lenovo", None);
    platform.service_root.oem_keys = vec!["Ami".to_string()];
    platform
}

fn lenovo_gb300() -> PlatformIdentity {
    with_chassis(
        with_system(
            identity("AMI", Some("AMI Redfish Server")),
            SystemIdentity {
                id: "System_0".to_string(),
                manufacturer: Some("Lenovo".to_string()),
                model: Some("HG634N_V2".to_string()),
                ..SystemIdentity::default()
            },
        ),
        ChassisIdentity {
            id: "HGX_Chassis_0".to_string(),
            model: Some("NVIDIA GB300".to_string()),
            ..ChassisIdentity::default()
        },
    )
}

fn viking() -> PlatformIdentity {
    with_system(
        identity("AMI", Some("AMI Redfish Server")),
        SystemIdentity {
            id: "DGX".to_string(),
            ..SystemIdentity::default()
        },
    )
}

fn dgx_without_bmc_manager() -> PlatformIdentity {
    let mut platform = viking();
    platform.manager = Some(ManagerIdentity {
        id: "Self".to_string(),
        ..ManagerIdentity::default()
    });
    platform
}

fn nvswitch() -> PlatformIdentity {
    with_chassis(
        identity("NVIDIA", Some("P3809")),
        ChassisIdentity {
            id: "MGX_NVSwitch_0".to_string(),
            ..ChassisIdentity::default()
        },
    )
}

fn power_shelf(vendor: &str, chassis_id: &str, manufacturer: &str) -> PlatformIdentity {
    with_chassis(
        identity(vendor, None),
        ChassisIdentity {
            id: chassis_id.to_string(),
            manufacturer: Some(manufacturer.to_string()),
            ..ChassisIdentity::default()
        },
    )
}

fn delta() -> PlatformIdentity {
    power_shelf("Delta Electronics Inc.", "chassis", "DELTA")
}

fn liteon() -> PlatformIdentity {
    power_shelf(
        "Lite-On Technology Corp.",
        "powershelf",
        "LITE-ON TECHNOLOGY CORP.",
    )
}

/// As a captured Supermicro GB300 tray reports it: the selected system is
/// `GB NVL`, and GB300 is named on the HGX baseboard's GPU chassis.
fn supermicro_gb300() -> PlatformIdentity {
    with_chassis(
        with_system(
            identity("Supermicro", Some("GB NVL")),
            SystemIdentity {
                id: "System_0".to_string(),
                manufacturer: Some("Supermicro".to_string()),
                model: Some("GB NVL".to_string()),
                ..SystemIdentity::default()
            },
        ),
        ChassisIdentity {
            id: "HGX_GPU_0".to_string(),
            model: Some("NVIDIA GB300".to_string()),
            ..ChassisIdentity::default()
        },
    )
}

fn ars121l() -> PlatformIdentity {
    with_system(
        identity("Supermicro", Some("Super Server")),
        SystemIdentity {
            id: "1".to_string(),
            model: Some("ARS-121L-DNR".to_string()),
            ..SystemIdentity::default()
        },
    )
}

fn bluefield2() -> PlatformIdentity {
    with_chassis(
        identity("Nvidia", Some("Nvidia-BMCMezz")),
        ChassisIdentity {
            id: "Card1".to_string(),
            model: Some("Bluefield 2 DPU 25GbE".to_string()),
            ..ChassisIdentity::default()
        },
    )
}

fn bluefield3() -> PlatformIdentity {
    identity("Nvidia", Some("BlueField-3 DPU"))
}

/// A Supermicro system whose NVIDIA processor module reports `model` and
/// `part_number`.
fn mgx_c2(model: Option<&str>, part_number: Option<&str>) -> PlatformIdentity {
    with_chassis(
        identity("Supermicro", None),
        ChassisIdentity {
            id: "PG535".to_string(),
            manufacturer: Some("NVIDIA".to_string()),
            model: model.map(str::to_string),
            part_number: part_number.map(str::to_string),
        },
    )
}

fn with_manager_firmware(mut platform: PlatformIdentity, firmware: &str) -> PlatformIdentity {
    if let Some(manager) = platform.manager.as_mut() {
        manager.firmware = Some(firmware.to_string());
    }
    platform
}

fn sr675_v3_ovx() -> PlatformIdentity {
    with_system(
        identity("Lenovo", None),
        SystemIdentity {
            id: "1".to_string(),
            sku: Some("7D9RCTOLWW".to_string()),
            ..SystemIdentity::default()
        },
    )
}

fn with_firmware_inventory(
    mut platform: PlatformIdentity,
    entries: &[(&str, &str)],
) -> PlatformIdentity {
    platform.firmware_inventory = entries
        .iter()
        .map(|(id, version)| FirmwareInventoryIdentity {
            id: (*id).to_string(),
            version: Some((*version).to_string()),
        })
        .collect();
    platform
}

fn driver(driver: Driver) -> CapabilitySelection {
    CapabilitySelection::Driver(driver)
}

/// Resolves with the compiled catalogue's defaults, as the runtime does.
fn resolve(identity: &PlatformIdentity) -> ResolvedSelection {
    built_in_rules()
        .resolve(identity, &Drivers::default_map())
        .expect("rules resolve")
}

/// Platforms no other test resolves still select one rule per capability.
#[test]
fn supported_platforms_resolve_without_ambiguity() {
    let identities = [
        liteon(),
        identity("Dell", None),
        identity("HPE", None),
        identity("Supermicro", Some("Super Server")),
        identity("AMI", Some("AMI Redfish Server")),
        identity("NVIDIA", Some("GB BMC")),
        identity("NVIDIA", Some("BlueField-4")),
        identity("WIWYNN", Some("GB200 NVL")),
    ];

    for identity in identities {
        resolve(&identity);
    }
}

#[test]
fn narrower_identity_outranks_broader_rules() {
    struct Case {
        scenario: &'static str,
        identity: PlatformIdentity,
        expect: Vec<(Capability, CapabilitySelection)>,
    }
    let cases = [
        Case {
            scenario: "Supermicro GB NVL tray takes the OpenBMC drivers",
            identity: identity("Supermicro", Some("GB NVL")),
            expect: vec![
                (Power, driver(NvidiaOpenBmcPower)),
                (Lockdown, Unsupported),
                (BmcControl, Standard),
                (Console, driver(NvidiaOpenBmcConsole)),
                (Bios, driver(NvidiaGbx00Bios)),
            ],
        },
        Case {
            scenario: "Supermicro GB300",
            identity: supermicro_gb300(),
            expect: vec![
                (Bios, driver(SupermicroGb300Bios)),
                (SecureBoot, Unsupported),
            ],
        },
        Case {
            scenario: "Vera Rubin takes its SuperNIC driver",
            identity: identity("NVIDIA", Some("VR NVL72")),
            expect: vec![
                (SuperNic, driver(NvidiaVeraRubinSuperNic)),
                (Bios, driver(NvidiaVeraRubinBios)),
            ],
        },
        Case {
            scenario: "DGX Viking",
            identity: viking(),
            expect: vec![
                (Accounts, driver(NvidiaVikingAccounts)),
                (Bios, driver(NvidiaVikingBios)),
                (SuperNic, Unsupported),
            ],
        },
        Case {
            scenario: "DGX without a BMC manager is plain AMI",
            identity: dgx_without_bmc_manager(),
            expect: vec![(Bios, driver(AmiMegaRacBios))],
        },
        Case {
            scenario: "BlueField-2",
            identity: bluefield2(),
            expect: vec![(Dpu, driver(NvidiaBlueField2Dpu))],
        },
        Case {
            scenario: "BlueField-4",
            identity: identity("Nvidia", Some("BlueField-4")),
            expect: vec![
                (Dpu, driver(NvidiaBlueField4Dpu)),
                (Power, driver(NvidiaBlueField4Power)),
                (Bios, driver(NvidiaBlueField4Bios)),
                (BootOrder, driver(NvidiaBlueFieldBootOrder)),
            ],
        },
        Case {
            scenario: "NVSwitch tray shadows GH200",
            identity: nvswitch(),
            expect: vec![
                (Accounts, driver(NvidiaSwitchAccounts)),
                (Bios, driver(NvidiaSwitchBios)),
                (Power, Standard),
                (Lockdown, Unsupported),
            ],
        },
        Case {
            scenario: "GH200",
            identity: identity("NVIDIA", Some("P3809")),
            expect: vec![
                (Accounts, driver(NvidiaGh200Accounts)),
                (Power, driver(NvidiaGh200Power)),
                (Firmware, Standard),
                (Bios, driver(NvidiaGh200Bios)),
            ],
        },
        Case {
            scenario: "Delta power shelf",
            identity: delta(),
            expect: vec![(Lockdown, Unsupported)],
        },
        Case {
            scenario: "Lenovo AMI shadows XCC",
            identity: lenovo_ami(),
            expect: vec![
                (Lockdown, driver(LenovoAmiLockdown)),
                (Accounts, driver(AmiMegaRacAccounts)),
                (Power, Standard),
                (SecureBoot, Unsupported),
            ],
        },
        Case {
            scenario: "Lenovo XCC",
            identity: identity("Lenovo", None),
            expect: vec![(Lockdown, driver(LenovoXccLockdown))],
        },
        Case {
            scenario: "Lenovo GB300",
            identity: lenovo_gb300(),
            expect: vec![
                (Lockdown, driver(LenovoGb300Lockdown)),
                (Bios, driver(LenovoGb300Bios)),
                (Attestation, driver(NvidiaHgxAttestation)),
            ],
        },
    ];

    for case in cases {
        let resolved = resolve(&case.identity);
        for (capability, expected) in case.expect {
            assert_eq!(
                resolved.drivers.get(capability),
                &expected,
                "{}: {capability}",
                case.scenario
            );
        }
    }
}

#[test]
fn firmware_and_model_resolve_to_their_quirks() {
    struct Case {
        scenario: &'static str,
        identity: PlatformIdentity,
        expect: Vec<Quirk>,
    }
    let cases = [
        Case {
            scenario: "BlueField-3 just before NIC-mode firmware",
            identity: with_manager_firmware(bluefield3(), "BF-23.10-4"),
            expect: vec![
                BlueFieldNicModeUnreadable,
                BlueFieldSpacedBiosAttributeNames,
            ],
        },
        Case {
            scenario: "BlueField-3 on the first NIC-mode firmware",
            identity: with_manager_firmware(bluefield3(), "BF-23.10-5"),
            expect: vec![BlueFieldNicModeBiosError, BlueFieldSpacedBiosAttributeNames],
        },
        Case {
            scenario: "BlueField-3 on the firmware whose OEM resource times out",
            identity: with_manager_firmware(bluefield3(), "BF-24.04-5"),
            expect: vec![
                BlueFieldNicModeBiosError,
                BlueFieldOemTimeoutInNicMode,
                BlueFieldSpacedBiosAttributeNames,
            ],
        },
        Case {
            scenario: "BlueField-3 on the last firmware answering BIOS reads with a 500",
            identity: with_manager_firmware(bluefield3(), "BF-24.07-13"),
            expect: vec![BlueFieldNicModeBiosError, BlueFieldSpacedBiosAttributeNames],
        },
        Case {
            scenario: "BlueField-3 once BIOS reads succeed in NIC mode",
            identity: with_manager_firmware(bluefield3(), "BF-24.07-14"),
            expect: vec![BlueFieldSpacedBiosAttributeNames],
        },
        Case {
            scenario: "BlueField-3 once BIOS attribute names lost their spaces",
            identity: with_manager_firmware(bluefield3(), "BF-24.10-10"),
            expect: vec![],
        },
        Case {
            scenario: "BlueField-2 before NIC-mode firmware",
            identity: with_manager_firmware(bluefield2(), "BF-23.09-9"),
            expect: vec![
                BlueFieldNicModeUnreadable,
                BlueFieldSpacedBiosAttributeNames,
            ],
        },
        Case {
            scenario: "BlueField-2 sharing the BlueField-3 BMC product on the OEM-timeout firmware",
            identity: with_manager_firmware(bluefield2(), "BF-24.04-5"),
            expect: vec![
                BlueFieldNicModeBiosError,
                BlueFieldOemTimeoutInNicMode,
                BlueFieldSpacedBiosAttributeNames,
            ],
        },
        Case {
            scenario: "BlueField-2 once BIOS reads succeed in NIC mode",
            identity: with_manager_firmware(bluefield2(), "BF-24.07-14"),
            expect: vec![BlueFieldSpacedBiosAttributeNames],
        },
        Case {
            scenario: "MGX C2 by processor module model on firmware exposing IPMIHostInterface",
            identity: with_manager_firmware(mgx_c2(Some("PG535-A00"), None), "01.05.01"),
            expect: vec![SupermicroMgxC2, SupermicroIpmiHostInterface],
        },
        Case {
            scenario: "MGX C2 by processor module part number on older firmware",
            identity: with_manager_firmware(mgx_c2(None, Some("699-2G535-0200-310")), "01.04.09"),
            expect: vec![SupermicroMgxC2],
        },
        Case {
            scenario: "MGX C2 whose manager reports no firmware",
            identity: mgx_c2(Some("PG535-A00"), None),
            expect: vec![SupermicroMgxC2],
        },
        Case {
            scenario: "SR675 V3 OVX at the firmware pair where ForceRestart hangs",
            identity: with_firmware_inventory(
                sr675_v3_ovx(),
                &[("UEFI", "7.10"), ("BMC-Primary", "9.10")],
            ),
            expect: vec![LenovoForceRestartHangs],
        },
        Case {
            scenario: "SR675 V3 OVX on other UEFI firmware",
            identity: with_firmware_inventory(
                sr675_v3_ovx(),
                &[("UEFI", "7.11"), ("BMC-Primary", "9.10")],
            ),
            expect: vec![],
        },
        Case {
            scenario: "Viking at its lockdown firmware minimums or newer",
            identity: with_firmware_inventory(
                viking(),
                &[("HostBIOS_0", "01.01.03"), ("HostBMC_0", "23.11.21")],
            ),
            expect: vec![RedfishRestartCutsDpuPower, VikingLockdownFirmware],
        },
        Case {
            scenario: "Viking BMC firmware below the lockdown minimum",
            identity: with_firmware_inventory(
                viking(),
                &[("HostBIOS_0", "01.01.03"), ("HostBMC_0", "23.11.08")],
            ),
            expect: vec![RedfishRestartCutsDpuPower],
        },
        Case {
            scenario: "Viking reporting no firmware inventory",
            identity: viking(),
            expect: vec![RedfishRestartCutsDpuPower],
        },
        Case {
            scenario: "Lenovo SR650 V4",
            identity: with_system(
                identity("Lenovo", None),
                SystemIdentity {
                    id: "1".to_string(),
                    model: Some("ThinkSystem SR650 V4".to_string()),
                    ..SystemIdentity::default()
                },
            ),
            expect: vec![RedfishRestartCutsDpuPower],
        },
        Case {
            scenario: "Dell XE9680",
            identity: with_system(
                identity("Dell", None),
                SystemIdentity {
                    id: "System.Embedded.1".to_string(),
                    model: Some("PowerEdge XE9680".to_string()),
                    ..SystemIdentity::default()
                },
            ),
            expect: vec![DellUefiPasswordClearJobFails],
        },
        Case {
            scenario: "Supermicro ARS-121L",
            identity: ars121l(),
            expect: vec![SupermicroHostInterfaceRequired],
        },
    ];

    for case in cases {
        assert_eq!(
            resolve(&case.identity).quirks,
            case.expect.into_iter().collect(),
            "{}",
            case.scenario
        );
    }
}
