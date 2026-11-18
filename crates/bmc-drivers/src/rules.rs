/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! The compiled platform rules mapping identity to capability drivers.
//!
//! Rules are declared from broad to narrow: vendor-wide evidence first,
//! then BMC product, then exact model or firmware. Precedence is derived from
//! the identity fields a rule reads, so declaration order carries no weight;
//! it only keeps the file readable. Tests prove every rule names a compiled
//! driver of the right capability.

use bmc_platform::{Capability, IdentityField, IdentityMatcher as Match, MatchPattern, Quirk};

use crate::drivers::Driver::*;
use crate::selection::{Rule, RuleError, Rules};

#[cfg(test)]
mod tests;

/// The compiled rules.
pub fn built_in_rules() -> Rules {
    Rules::new(built_ins()).expect("compiled rules must be valid")
}

/// The compiled rules plus deployment overrides, which outrank every
/// built-in rule and can never be shadowed by one.
pub fn rules_with_overrides(overrides: impl IntoIterator<Item = Rule>) -> Result<Rules, RuleError> {
    Rules::with_overrides(built_ins(), overrides)
}

/// The products of BlueField-3 BMCs; BlueField-2 cards are told apart by
/// their chassis model.
const BLUEFIELD3_PRODUCTS: &[&str] = &["Nvidia-BMCMezz", "BlueField-3 DPU"];

/// The model prefix and part-number fragment of the NVIDIA processor module
/// in Supermicro MGX C2 systems.
const MGX_C2_MODEL_PREFIX: &str = "PG535";
const MGX_C2_PART_NUMBER: &str = "2G535";

/// Any chassis manufactured by NVIDIA.
fn nvidia_chassis() -> Match {
    Match::new(
        IdentityField::ChassisManufacturer,
        MatchPattern::ExactAsciiCaseInsensitive("NVIDIA".to_string()),
    )
}

#[allow(clippy::vec_init_then_push)] // rustfmt strips blank lines between `vec!` elements.
fn built_ins() -> Vec<Rule> {
    let mut rules = Vec::new();

    // ---- Vendor rules: ServiceRoot vendor, system or chassis manufacturer ----

    rules.push(Rule::new("dell", [Match::vendor("Dell")]).drivers([
        DellIdracPower,
        DellIdracBmcControl,
        DellIdracBios,
        DellIdracBootOrder,
        DellIdracLockdown,
        DellIdracAccounts,
        DellIdracFirmware,
        DellIdracBossStorage,
        DellIdracConsole,
    ]));

    rules.push(Rule::new("hpe", [Match::vendor("HPE")]).drivers([
        HpeIloPower,
        HpeIloBmcControl,
        HpeIloBios,
        HpeIloBootOrder,
        HpeIloLockdown,
        HpeIloAccounts,
        HpeIloConsole,
    ]));

    rules.push(Rule::new("lenovo-xcc", [Match::vendor("Lenovo")]).drivers([
        LenovoXccPower,
        LenovoXccBios,
        LenovoXccBootOrder,
        LenovoXccFirmware,
        LenovoXccLockdown,
        LenovoXccAccounts,
        LenovoXccConsole,
    ]));

    rules.push(
        Rule::new("supermicro", [Match::vendor("Supermicro")]).drivers([
            SupermicroSmcPower,
            SupermicroSmcBmcControl,
            SupermicroSmcBios,
            SupermicroSmcFirmware,
            SupermicroSmcBootOrder,
            SupermicroSmcLockdown,
            SupermicroBmcConsole,
        ]),
    );

    rules.push(Rule::new("ami-megarac", [Match::vendor("AMI")]).drivers([
        AmiMegaRacBmcControl,
        AmiMegaRacBios,
        AmiMegaRacBootOrder,
        AmiMegaRacLockdown,
        AmiMegaRacAccounts,
        AmiMegaRacConsole,
    ]));

    // Lenovo HS350x-class trays run AMI firmware behind a Lenovo service
    // root; the extra OEM-key matcher outranks the plain XCC vendor
    // rule. The AMI firmware takes AMI accounts and standard resets, not
    // XCC's, and has no XCC AC power cycle. Its SecureBoot resource reports
    // a null SecureBootEnable and no current-boot state.
    rules.push(
        Rule::new(
            "lenovo-ami",
            [Match::vendor("Lenovo"), Match::oem_key("Ami")],
        )
        .drivers([
            AmiMegaRacBmcControl,
            AmiMegaRacBios,
            AmiMegaRacBootOrder,
            LenovoAmiLockdown,
            AmiMegaRacAccounts,
            AmiMegaRacFirmware,
            LenovoAmiConsole,
        ])
        .standard([Capability::Power])
        .unsupported([Capability::SecureBoot]),
    );

    // Power shelves have no ServiceRoot vendor; their chassis manufacturer
    // identifies them.
    rules.push(
        Rule::new("delta-power-shelf", [Match::chassis_manufacturer("Delta")])
            .drivers([DeltaPowerShelfPower, DeltaPowerShelfAccounts])
            .unsupported([
                Capability::Bios,
                Capability::BootOrder,
                Capability::SecureBoot,
                Capability::Lockdown,
                Capability::Firmware,
                Capability::Attestation,
            ]),
    );

    rules.push(
        Rule::new(
            "liteon-power-shelf",
            [Match::chassis_manufacturer("Lite-On")],
        )
        .drivers([
            LiteOnPowerShelfPower,
            LiteOnPowerShelfAccounts,
            LiteOnPowerShelfBios,
        ])
        .unsupported([
            Capability::BootOrder,
            Capability::SecureBoot,
            Capability::Lockdown,
            Capability::Attestation,
        ]),
    );

    // Wiwynn's GB200 NVL trays run NVIDIA OpenBMC under their own vendor.
    // OpenBMC has no lockdown.
    rules.push(
        Rule::new("nvidia-gbx00-wiwynn", [Match::vendor("Wiwynn")])
            .drivers([
                NvidiaGbx00BootOrder,
                NvidiaOpenBmcPower,
                NvidiaOpenBmcFirmware,
                NvidiaOpenBmcAccounts,
                NvidiaHgxAttestation,
                NvidiaGbx00Bios,
                NvidiaOpenBmcConsole,
            ])
            .unsupported([Capability::Lockdown]),
    );

    // Giga Computing servers run AMI firmware behind an AMI service root;
    // only the system manufacturer tells them apart. Their BIOS spells its
    // settings differently, and their lockdown is the host interface alone.
    rules.push(
        Rule::new(
            "giga-computing-ami",
            [
                Match::vendor("AMI"),
                Match::contains_any_case(IdentityField::SystemManufacturer, "Giga Computing"),
            ],
        )
        .drivers([
            GigaComputingAmiBios,
            GigaComputingAmiConsole,
            GigaComputingAmiLockdown,
        ]),
    );

    // ---- Product rules: ServiceRoot product or Manager model ----

    // Supermicro's GB NVL trays also match the Supermicro vendor rule,
    // whose OEM factory reset and console do not apply to OpenBMC.
    rules.push(
        Rule::new(
            "nvidia-gbx00",
            [Match::product(&["GB BMC", "GB200 NVL", "GB NVL"])],
        )
        .drivers([
            NvidiaGbx00BootOrder,
            NvidiaOpenBmcPower,
            NvidiaOpenBmcFirmware,
            NvidiaOpenBmcAccounts,
            NvidiaHgxAttestation,
            NvidiaGbx00Bios,
            NvidiaOpenBmcConsole,
        ])
        .standard([Capability::BmcControl])
        .unsupported([Capability::Lockdown]),
    );

    rules.push(
        Rule::new("nvidia-vera", [Match::product(&["VR NVL72"])])
            .drivers([
                NvidiaVeraRubinBootOrder,
                NvidiaVeraRubinBmcControl,
                NvidiaOpenBmcPower,
                NvidiaOpenBmcFirmware,
                NvidiaOpenBmcAccounts,
                NvidiaHgxAttestation,
                NvidiaVeraRubinBios,
                NvidiaOpenBmcConsole,
                NvidiaVeraRubinSuperNic,
            ])
            .unsupported([Capability::Lockdown]),
    );

    // GH200 firmware updates take the caller's parameters unchanged.
    rules.push(
        Rule::new("nvidia-gh", [Match::product(&["P3809"])])
            .drivers([
                NvidiaGh200BootOrder,
                NvidiaGh200Bios,
                NvidiaGh200Power,
                NvidiaGh200Accounts,
                NvidiaOpenBmcConsole,
            ])
            .unsupported([Capability::Lockdown, Capability::Attestation]),
    );

    rules.push(
        Rule::new(
            "bluefield",
            [Match::product(&[
                "Nvidia-BMCMezz",
                "BlueField-3 DPU",
                "BlueField-4",
                "B4240V",
            ])],
        )
        .drivers([
            NvidiaBlueFieldBios,
            NvidiaBlueFieldBootOrder,
            NvidiaBlueFieldAccounts,
            NvidiaBlueFieldDpu,
            NvidiaBlueFieldConsole,
        ]),
    );

    // BlueField-4 keeps its DPU mode and host privileges on the network
    // adapter rather than the system and BIOS, and powers its Arm cores
    // through the card chassis rather than the system. The vendor matcher
    // outranks the `bluefield` rule, which shares the product precedence.
    rules.push(
        Rule::new(
            "nvidia-bluefield4",
            [
                Match::vendor("Nvidia"),
                Match::product(&["BlueField-4", "B4240V"]),
            ],
        )
        .drivers([
            NvidiaBlueField4Dpu,
            NvidiaBlueField4Power,
            NvidiaBlueField4Bios,
        ]),
    );

    // XCC 3 has no OEM boot settings; it orders network adapters through BIOS
    // attributes.
    rules.push(
        Rule::new(
            "lenovo-xcc3",
            [
                Match::vendor("Lenovo"),
                Match::exact(IdentityField::ManagerModel, "Lenovo XClarity Controller 3"),
            ],
        )
        .drivers([LenovoXcc3BootOrder]),
    );

    // ---- Model rules: exact system, chassis, or firmware evidence ----

    // GB NVSwitch trays share the GH200 service root; only their switch
    // chassis tells them apart. Their BIOS takes only password changes, and
    // they have no secure boot, lockdown, AC power cycle, or attestation.
    rules.push(
        Rule::new(
            "nvidia-switch",
            [
                Match::product(&["P3809"]),
                Match::exact(IdentityField::ChassisId, "MGX_NVSwitch_0"),
            ],
        )
        .drivers([
            NvidiaSwitchBootOrder,
            NvidiaSwitchAccounts,
            NvidiaSwitchBios,
        ])
        .standard([Capability::Power])
        .unsupported([
            Capability::SecureBoot,
            Capability::Lockdown,
            Capability::Firmware,
            Capability::Attestation,
        ]),
    );

    // BlueField-2 identifies itself only through its card chassis model.
    rules.push(
        Rule::new(
            "nvidia-bluefield2",
            [Match::contains_any_case(
                IdentityField::ChassisModel,
                "BlueField 2",
            )],
        )
        .drivers([NvidiaBlueField2Dpu]),
    );

    // GB200 trays from every vendor name GB200 in their system model, which
    // is how the machine controller tells them apart from GB300.
    rules.push(
        Rule::new(
            "nvidia-gb200",
            [Match::contains_any_case(
                IdentityField::SystemModel,
                "GB200",
            )],
        )
        .drivers([NvidiaGb200BmcControl]),
    );

    // Lenovo GB300 trays run AMI firmware; the GB300 model is on the GPU
    // baseboard, not the selected Lenovo host system. The baseboard's GPUs
    // attest through HGX ComponentIntegrity.
    rules.push(
        Rule::new(
            "lenovo-gb300",
            [
                Match::vendor("AMI"),
                Match::contains(IdentityField::SystemManufacturer, "Lenovo"),
                Match::contains(IdentityField::ChassisModel, "GB300"),
            ],
        )
        .drivers([
            LenovoGb300Bios,
            LenovoGb300BootOrder,
            LenovoGb300Lockdown,
            LenovoGb300Console,
            NvidiaHgxAttestation,
        ]),
    );

    // DGX Viking runs AMI firmware and identifies itself by its system and
    // manager ids.
    rules.push(
        Rule::new(
            "nvidia-viking",
            [
                Match::vendor("AMI"),
                Match::exact(IdentityField::SystemId, "DGX"),
                Match::exact(IdentityField::ManagerId, "BMC"),
            ],
        )
        .drivers([
            AmiMegaRacBmcControl,
            NvidiaVikingBios,
            NvidiaVikingBootOrder,
            NvidiaVikingLockdown,
            NvidiaVikingAccounts,
            NvidiaVikingFirmware,
            NvidiaVikingConsole,
        ]),
    );

    // Supermicro GB300 firmware exposes SecureBoot without SecureBootEnable.
    // The selected system reports `GB NVL`; GB300 is named on the GPU chassis
    // of the separate HGX baseboard system.
    rules.push(
        Rule::new(
            "supermicro-gb300",
            [
                Match::vendor("Supermicro"),
                Match::contains_any_case(IdentityField::SystemManufacturer, "Supermicro"),
                Match::contains(IdentityField::ChassisModel, "GB300"),
            ],
        )
        .drivers([SupermicroGb300Bios])
        .unsupported([Capability::SecureBoot]),
    );

    // ---- Quirk rules: every matching rule's quirks apply ----

    // SR650 V4 and DGX Viking hosts cut DPU power on a Redfish restart.
    rules.push(
        Rule::new(
            "lenovo-sr650-v4-redfish-restart-cuts-dpu-power",
            [
                Match::vendor("Lenovo"),
                Match::contains(IdentityField::SystemModel, "SR650 V4"),
            ],
        )
        .quirks([Quirk::RedfishRestartCutsDpuPower]),
    );
    rules.push(
        Rule::new(
            "nvidia-viking-redfish-restart-cuts-dpu-power",
            [
                Match::vendor("AMI"),
                Match::exact(IdentityField::SystemId, "DGX"),
                Match::exact(IdentityField::ManagerId, "BMC"),
            ],
        )
        .quirks([Quirk::RedfishRestartCutsDpuPower]),
    );

    // XE9680's iDRAC fails the configuration job of a setup password clear.
    rules.push(
        Rule::new(
            "dell-xe9680-uefi-password-clear-job-fails",
            [
                Match::vendor("Dell"),
                Match::contains(IdentityField::SystemModel, "XE9680"),
            ],
        )
        .quirks([Quirk::DellUefiPasswordClearJobFails]),
    );

    // ARS-121L-DNR loses BMC reachability when its host interface is disabled.
    rules.push(
        Rule::new(
            "supermicro-ars121l-host-interface-required",
            [
                Match::vendor("Supermicro"),
                Match::contains(IdentityField::SystemModel, "ARS-121L-DNR"),
            ],
        )
        .quirks([Quirk::SupermicroHostInterfaceRequired]),
    );

    // A standard ForceRestart can hang on the SR675 V3 OVX at UEFI 7.10 with
    // BMC 9.10.
    rules.push(
        Rule::new(
            "lenovo-sr675-v3-ovx-force-restart-hangs",
            [
                Match::exact(IdentityField::SystemSku, "7D9RCTOLWW"),
                Match::version_equal(IdentityField::FirmwareInventory("UEFI".to_string()), "7.10"),
                Match::version_equal(
                    IdentityField::FirmwareInventory("BMC-Primary".to_string()),
                    "9.10",
                ),
            ],
        )
        .quirks([Quirk::LenovoForceRestartHangs]),
    );

    // Viking lockdown needs at least host BIOS 1.01.03 and BMC 23.11.09.
    rules.push(
        Rule::new(
            "nvidia-viking-lockdown-firmware",
            [
                Match::vendor("AMI"),
                Match::exact(IdentityField::SystemId, "DGX"),
                Match::exact(IdentityField::ManagerId, "BMC"),
                Match::version_at_least(
                    IdentityField::FirmwareInventory("HostBIOS_0".to_string()),
                    "1.01.03",
                ),
                Match::version_at_least(
                    IdentityField::FirmwareInventory("HostBMC_0".to_string()),
                    "23.11.09",
                ),
            ],
        )
        .quirks([Quirk::VikingLockdownFirmware]),
    );

    // BlueField BMC firmware before BF-23.10-5 cannot read or switch NIC mode
    // through Redfish.
    rules.push(
        Rule::new(
            "nvidia-bluefield3-nic-mode-unreadable",
            [
                Match::product(BLUEFIELD3_PRODUCTS),
                Match::version_below(IdentityField::ManagerFirmware, "BF-23.10-5"),
            ],
        )
        .quirks([Quirk::BlueFieldNicModeUnreadable]),
    );
    rules.push(
        Rule::new(
            "nvidia-bluefield2-nic-mode-unreadable",
            [
                Match::contains_any_case(IdentityField::ChassisModel, "BlueField 2"),
                Match::version_below(IdentityField::ManagerFirmware, "BF-23.10-5"),
            ],
        )
        .quirks([Quirk::BlueFieldNicModeUnreadable]),
    );

    // BlueField BMC firmware from BF-23.10-5 until BF-24.07-14 answers a BIOS
    // read in NIC mode with a 500 that still reports NIC mode.
    rules.push(
        Rule::new(
            "nvidia-bluefield3-nic-mode-bios-error",
            [
                Match::product(BLUEFIELD3_PRODUCTS),
                Match::version_at_least(IdentityField::ManagerFirmware, "BF-23.10-5"),
                Match::version_below(IdentityField::ManagerFirmware, "BF-24.07-14"),
            ],
        )
        .quirks([Quirk::BlueFieldNicModeBiosError]),
    );
    rules.push(
        Rule::new(
            "nvidia-bluefield2-nic-mode-bios-error",
            [
                Match::contains_any_case(IdentityField::ChassisModel, "BlueField 2"),
                Match::version_at_least(IdentityField::ManagerFirmware, "BF-23.10-5"),
                Match::version_below(IdentityField::ManagerFirmware, "BF-24.07-14"),
            ],
        )
        .quirks([Quirk::BlueFieldNicModeBiosError]),
    );

    // On BF-24.04-5 the BlueField-3 system Oem/Nvidia resource times out on a
    // DPU in NIC mode. BlueField-2 cards sharing the BMC product match too,
    // but their driver never reads that resource.
    rules.push(
        Rule::new(
            "nvidia-bluefield3-oem-timeout",
            [
                Match::product(BLUEFIELD3_PRODUCTS),
                Match::version_equal(IdentityField::ManagerFirmware, "BF-24.04-5"),
            ],
        )
        .quirks([Quirk::BlueFieldOemTimeoutInNicMode]),
    );

    // BlueField BMC firmware before 24.10 spells some BIOS attributes with spaces.
    rules.push(
        Rule::new(
            "nvidia-bluefield3-spaced-bios-attribute-names",
            [
                Match::product(BLUEFIELD3_PRODUCTS),
                Match::version_below(IdentityField::ManagerFirmware, "BF-24.10"),
            ],
        )
        .quirks([Quirk::BlueFieldSpacedBiosAttributeNames]),
    );
    rules.push(
        Rule::new(
            "nvidia-bluefield2-spaced-bios-attribute-names",
            [
                Match::contains_any_case(IdentityField::ChassisModel, "BlueField 2"),
                Match::version_below(IdentityField::ManagerFirmware, "BF-24.10"),
            ],
        )
        .quirks([Quirk::BlueFieldSpacedBiosAttributeNames]),
    );

    // Supermicro MGX C2 systems carry an NVIDIA PG535 processor module, known
    // by its model or its part number. Their BMC exposes the system
    // IPMIHostInterface from firmware 01.05.01.
    rules.push(
        Rule::new(
            "supermicro-mgx-c2",
            [
                Match::vendor("Supermicro"),
                nvidia_chassis(),
                Match::new(
                    IdentityField::ChassisModel,
                    MatchPattern::Prefix(MGX_C2_MODEL_PREFIX.to_string()),
                ),
            ],
        )
        .quirks([Quirk::SupermicroMgxC2]),
    );
    rules.push(
        Rule::new(
            "supermicro-mgx-c2-by-part-number",
            [
                Match::vendor("Supermicro"),
                nvidia_chassis(),
                Match::contains(IdentityField::ChassisPartNumber, MGX_C2_PART_NUMBER),
            ],
        )
        .quirks([Quirk::SupermicroMgxC2]),
    );
    rules.push(
        Rule::new(
            "supermicro-mgx-c2-ipmi-host-interface",
            [
                Match::vendor("Supermicro"),
                nvidia_chassis(),
                Match::new(
                    IdentityField::ChassisModel,
                    MatchPattern::Prefix(MGX_C2_MODEL_PREFIX.to_string()),
                ),
                Match::version_at_least(IdentityField::ManagerFirmware, "01.05.01"),
            ],
        )
        .quirks([Quirk::SupermicroIpmiHostInterface]),
    );
    rules.push(
        Rule::new(
            "supermicro-mgx-c2-by-part-number-ipmi-host-interface",
            [
                Match::vendor("Supermicro"),
                nvidia_chassis(),
                Match::contains(IdentityField::ChassisPartNumber, MGX_C2_PART_NUMBER),
                Match::version_at_least(IdentityField::ManagerFirmware, "01.05.01"),
            ],
        )
        .quirks([Quirk::SupermicroIpmiHostInterface]),
    );

    rules
}
