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

use carbide_test_support::value_scenarios;
use model::firmware::{Firmware, FirmwareComponentType, HostFirmwareConfig};

use crate::config::*;

const LENOVO_CFG: &str = r#"
model = "ThinkSystem HS350X V3"
vendor = "Lenovo"

[components.bmc]
current_version_reported_as = "BMCImage1"
preingest_upgrade_when_below = "1.27.260418"

[[components.bmc.known_firmware]]
version = "1.27.260418"
filename = "/opt/carbide/firmware/lenovo-thinksystem_hs350x_v3-bmc-1.27.260418/lnvgy_fw_BMC_igc602j-1.27_anyos_noarch.ima"
default = true
"#;

const LENOVOAMI_CFG: &str = r#"
model = "ThinkSystem HS350X V3"
vendor = "LenovoAMI"

[components.bmc]
current_version_reported_as = "BMCImage1"

[[components.bmc.known_firmware]]
version = "1.28.260500"
filename = "/opt/carbide/firmware/lenovoami-thinksystem_hs350x_v3-bmc-1.28.260500/lnvgy_fw_BMC_igc602x-1.28_anyos_noarch.ima"
default = true
"#;

const LENOVO_RUNTIME_CFG: &str = r#"
model = "ThinkSystem HS350X V3"
vendor = "Lenovo"

[components.bmc]
current_version_reported_as = "BMCImage1"

[[components.bmc.known_firmware]]
version = "1.29.260700"
filename = "/opt/carbide/firmware/lenovo-thinksystem_hs350x_v3-bmc-1.29.260700/lnvgy_fw_BMC_igc602z-1.29_anyos_noarch.ima"
default = true
"#;

const DELL_METADATA_CFG: &str = r#"
model = "PowerEdge R750"
vendor = "Dell"
ordering = ["uefi", "bmc"]
explicit_start_needed = true

[components.uefi]
current_version_reported_as = "^Installed-.*__BIOS.Setup."
preingest_upgrade_when_below = "1.13.2"

[[components.uefi.known_firmware]]
version = "1.13.2"
filename = "/opt/carbide/firmware/dell-poweredge_r750-uefi-1.13.2/BIOS_T3H20_WN64_1.13.2.EXE"
default = true

[components.bmc]
current_version_reported_as = "^Installed-.*__iDRAC."
preingest_upgrade_when_below = "7.10.30.00"

[[components.bmc.known_firmware]]
version = "7.10.30.00"
filename = "/opt/carbide/firmware/dell-poweredge_r750-bmc-7.10.30.00/iDRAC.EXE"
default = true
"#;

const DELL_RUNTIME_CFG: &str = r#"
model = "PowerEdge R750"
vendor = "Dell"
ordering = ["cx7", "bmc"]
explicit_start_needed = false

[components.bmc]
current_version_reported_as = "^Runtime-BMC$"
preingest_upgrade_when_below = "7.20.00.00"

[[components.bmc.known_firmware]]
version = "7.10.30.00"
url = "https://firmware.example.invalid/bmc-7.10.30.00-runtime.exe"
default = false

[[components.bmc.known_firmware]]
version = "7.20.00.00"
url = "https://firmware.example.invalid/bmc-7.20.00.00.exe"
default = true

[components.cx7]
current_version_reported_as = "^CX7_[0-9]+$"

[[components.cx7.known_firmware]]
version = "28.48.1000"
url = "https://firmware.example.invalid/cx7-28.48.1000.bin"
default = true
"#;

#[derive(Clone, Copy, Debug)]
enum LenovoLookupCase {
    DirectLenovo,
    LenovoAmiFallback,
    LenovoAmiOverride,
    MissingVendor,
}

#[derive(Debug, PartialEq)]
struct FirmwareLookupSummary {
    vendor: String,
    bmc_version: Option<String>,
}

fn config_with_overrides(overrides: &[&str]) -> FirmwareConfig {
    let mut config: FirmwareConfig = Default::default();
    for ovrd in overrides {
        config.add_test_override((*ovrd).to_string());
    }
    config
}

fn summarize_firmware(firmware: Firmware) -> FirmwareLookupSummary {
    FirmwareLookupSummary {
        vendor: firmware.vendor.to_string(),
        bmc_version: firmware
            .components
            .get(&FirmwareComponentType::Bmc)
            .and_then(|component| {
                component
                    .known_firmware
                    .iter()
                    .find(|firmware| firmware.default)
                    .or_else(|| component.known_firmware.first())
            })
            .map(|firmware| firmware.version.clone()),
    }
}

fn summarize_lenovo_lookup(case: LenovoLookupCase) -> Option<FirmwareLookupSummary> {
    let (overrides, vendor) = match case {
        LenovoLookupCase::DirectLenovo => (&[LENOVO_CFG][..], bmc_vendor::BMCVendor::Lenovo),
        LenovoLookupCase::LenovoAmiFallback => {
            (&[LENOVO_CFG][..], bmc_vendor::BMCVendor::LenovoAMI)
        }
        // The lookup summaries here intentionally ignore upgrade thresholds; this
        // case only checks that a vendor-specific LenovoAMI config wins.
        LenovoLookupCase::LenovoAmiOverride => (
            &[LENOVO_CFG, LENOVOAMI_CFG][..],
            bmc_vendor::BMCVendor::LenovoAMI,
        ),
        LenovoLookupCase::MissingVendor => (&[LENOVO_CFG][..], bmc_vendor::BMCVendor::Dell),
    };
    config_with_overrides(overrides)
        .create_snapshot()
        .find(vendor, "ThinkSystem HS350X V3")
        .map(summarize_firmware)
}

#[test]
fn merging_config() -> eyre::Result<()> {
    let cfg1 = r#"
    vendor = "Dell"
    model = "PowerEdge R750"
    ordering = ["uefi", "bmc"]


    [components.uefi]
    current_version_reported_as = "^Installed-.*__BIOS.Setup."
    preingest_upgrade_when_below = "1.13.2"

    [[components.uefi.known_firmware]]
    version = "1.13.2"
    url = "https://urm.nvidia.com/artifactory/sw-ngc-forge-cargo-local/misc/BIOS_T3H20_WN64_1.13.2.EXE"
    default = true
"#;
    let cfg2 = r#"
model = "PowerEdge R750"
vendor = "Dell"

[components.uefi]
current_version_reported_as = "^Installed-.*__BIOS.Setup."
preingest_upgrade_when_below = "1.13.3"

[[components.uefi.known_firmware]]
version = "1.13.3"
url = "https://urm.nvidia.com/artifactory/sw-ngc-forge-cargo-local/misc/BIOS_T3H20_WN64_1.13.2.EXE"
default = true

[components.bmc]
current_version_reported_as = "^Installed-.*__iDRAC."

[[components.bmc.known_firmware]]
version = "7.10.30.00"
filenames = ["/opt/carbide/iDRAC-with-Lifecycle-Controller_Firmware_HV310_WN64_7.10.30.00_A00.EXE", "/opt/carbide/iDRAC-with-Lifecycle-Controller_Firmware_HV310_WN64_7.10.30.00_A01.EXE"]
default = true
    "#;
    let mut config: FirmwareConfig = Default::default();
    config.add_test_override(cfg1.to_string());
    config.add_test_override(cfg2.to_string());

    println!("{config:#?}");
    let snapshot = config.create_snapshot();
    let server = snapshot.data.get("dell:poweredge r750").unwrap();
    assert_eq!(
        server
            .components
            .get(&FirmwareComponentType::Uefi)
            .unwrap()
            .known_firmware
            .len(),
        2
    );
    assert_eq!(
        server
            .components
            .get(&FirmwareComponentType::Bmc)
            .unwrap()
            .known_firmware
            .len(),
        1
    );
    assert_eq!(
        server
            .components
            .get(&FirmwareComponentType::Bmc)
            .unwrap()
            .known_firmware
            .first()
            .unwrap()
            .filenames
            .len(),
        2
    );
    assert_eq!(
        *server
            .components
            .get(&FirmwareComponentType::Uefi)
            .unwrap()
            .preingest_upgrade_when_below
            .as_ref()
            .unwrap(),
        "1.13.3".to_string()
    );
    Ok(())
}

#[test]
fn finds_lenovo_firmware_configs() {
    value_scenarios!(
        summarize_lenovo_lookup:
        "direct lookup" {
            LenovoLookupCase::DirectLenovo => Some(FirmwareLookupSummary {
                vendor: "lenovo".to_string(),
                bmc_version: Some("1.27.260418".to_string()),
            }),
        }

        "lenovoami fallback" {
            LenovoLookupCase::LenovoAmiFallback => Some(FirmwareLookupSummary {
                vendor: "lenovo".to_string(),
                bmc_version: Some("1.27.260418".to_string()),
            }),
        }

        "lenovoami override" {
            LenovoLookupCase::LenovoAmiOverride => Some(FirmwareLookupSummary {
                vendor: "lenovoami".to_string(),
                bmc_version: Some("1.28.260500".to_string()),
            }),
        }

        "unmatched vendor" {
            LenovoLookupCase::MissingVendor => None,
        }
    );
}

#[test]
fn create_snapshot_with_overrides_uses_runtime_default_for_matching_component() -> eyre::Result<()>
{
    let runtime_config = toml::from_str::<HostFirmwareConfig>(LENOVO_RUNTIME_CFG)?;

    let snapshot =
        config_with_overrides(&[LENOVO_CFG]).create_snapshot_with_overrides([runtime_config]);

    assert_eq!(
        snapshot
            .find(bmc_vendor::BMCVendor::Lenovo, "ThinkSystem HS350X V3")
            .map(summarize_firmware),
        Some(FirmwareLookupSummary {
            vendor: "lenovo".to_string(),
            bmc_version: Some("1.29.260700".to_string()),
        })
    );
    Ok(())
}

#[test]
fn create_snapshot_with_overrides_merges_runtime_config_with_metadata() -> eyre::Result<()> {
    let runtime_config = toml::from_str::<HostFirmwareConfig>(DELL_RUNTIME_CFG)?;

    let snapshot = config_with_overrides(&[DELL_METADATA_CFG])
        .create_snapshot_with_overrides([runtime_config]);
    let server = snapshot.data.get("dell:poweredge r750").unwrap();

    assert!(!server.explicit_start_needed);
    assert_eq!(
        server.ordering,
        vec![
            FirmwareComponentType::Uefi,
            FirmwareComponentType::Bmc,
            FirmwareComponentType::Cx7
        ]
    );

    let uefi = server.components.get(&FirmwareComponentType::Uefi).unwrap();
    assert_eq!(
        uefi.known_firmware
            .iter()
            .filter(|firmware| firmware.default)
            .map(|firmware| firmware.version.as_str())
            .collect::<Vec<_>>(),
        vec!["1.13.2"]
    );

    let bmc = server.components.get(&FirmwareComponentType::Bmc).unwrap();
    assert_eq!(
        bmc.current_version_reported_as
            .as_ref()
            .map(|regex| regex.as_str()),
        Some("^Runtime-BMC$")
    );
    assert_eq!(
        bmc.preingest_upgrade_when_below.as_deref(),
        Some("7.20.00.00")
    );
    assert_eq!(
        bmc.known_firmware
            .iter()
            .map(|firmware| {
                (
                    firmware.version.as_str(),
                    firmware.default,
                    firmware.filename.as_deref(),
                    firmware.url.as_deref(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "7.10.30.00",
                false,
                None,
                Some("https://firmware.example.invalid/bmc-7.10.30.00-runtime.exe")
            ),
            (
                "7.20.00.00",
                true,
                None,
                Some("https://firmware.example.invalid/bmc-7.20.00.00.exe")
            ),
        ]
    );

    let cx7 = server.components.get(&FirmwareComponentType::Cx7).unwrap();
    assert_eq!(cx7.known_firmware[0].version, "28.48.1000");

    Ok(())
}

#[test]
fn create_snapshot_with_overrides_preserves_metadata_explicit_start_when_runtime_omits()
-> eyre::Result<()> {
    let mut runtime_config = toml::from_str::<HostFirmwareConfig>(DELL_RUNTIME_CFG)?;
    runtime_config.explicit_start_needed = None;

    let snapshot = config_with_overrides(&[DELL_METADATA_CFG])
        .create_snapshot_with_overrides([runtime_config]);
    let server = snapshot.data.get("dell:poweredge r750").unwrap();

    assert!(server.explicit_start_needed);

    Ok(())
}

#[test]
fn cx7_component_config_parses_as_first_class_component() -> eyre::Result<()> {
    let cfg = r#"
model = "DGXH100"
vendor = "Nvidia"
ordering = ["hgxbmc", "combinedbmcuefi", "uefi", "bmc", "cx7"]

[components.cx7]
current_version_reported_as = "^CX7_[0-9]+$"

[[components.cx7.known_firmware]]
version = "28.47.2682"
filename = "/opt/carbide/firmware/nvidia-dgxh100-cx7-28.47.2682/cx7.bin"
filenames = ["/opt/carbide/firmware/nvidia-dgxh100-cx7-28.47.2682/cx7.bin"]
default = true
power_drains_needed = 1

[[components.cx7.known_firmware.files]]
filename = "/opt/carbide/firmware/nvidia-dgxh100-cx7-28.47.2682/cx7.bin"
sha256 = "abc123"

[[components.cx7.known_firmware.files]]
url = "https://firmware.example.invalid/cx7.bin"
sha256 = "def456"

[components.cx7.known_firmware.scout]
execution_timeout_seconds = 1800
artifact_download_timeout_seconds = 600
"#;
    let mut config: FirmwareConfig = Default::default();
    config.add_test_override(cfg.to_string());

    let snapshot = config.create_snapshot();
    let server = snapshot.data.get("nvidia:dgxh100").unwrap();
    assert_eq!(
        server.ordering.last().copied(),
        Some(FirmwareComponentType::Cx7)
    );

    let cx7 = server.components.get(&FirmwareComponentType::Cx7).unwrap();
    assert!(cx7.current_version_reported_as.is_some());
    let firmware = cx7.known_firmware.first().unwrap();
    assert_eq!(firmware.version, "28.47.2682");
    assert_eq!(firmware.power_drains_needed, Some(1));
    assert_eq!(firmware.files.len(), 2);
    assert_eq!(
        firmware.files[0].filename.as_deref(),
        Some("/opt/carbide/firmware/nvidia-dgxh100-cx7-28.47.2682/cx7.bin")
    );
    assert_eq!(firmware.files[1].filename, None);
    assert_eq!(
        firmware.files[1].url.as_deref(),
        Some("https://firmware.example.invalid/cx7.bin")
    );

    let scout = firmware.scout.as_ref().unwrap();
    assert_eq!(scout.execution_timeout_seconds, 1800);
    assert_eq!(scout.artifact_download_timeout_seconds, 600);
    assert!(scout.script.is_none());
    Ok(())
}
