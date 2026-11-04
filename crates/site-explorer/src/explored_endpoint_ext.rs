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

use carbide_network::{is_locally_administered_mac, sanitized_mac};
use mac_address::MacAddress;
use model::DpuModel;
use model::site_explorer::{
    EndpointExplorationReport, ExploredEndpoint, Service, is_bluefield_system,
};

/// Operations on the reports stored in an explored endpoint.
pub(super) trait ExploredEndpointExt {
    /// Identifies the MAC address that is used by the pf0 interface that
    /// the DPU exposes to the host.
    ///
    /// According "MAC and GUID allocation and assignment" document
    ///
    /// Ethernet only require allocation of MAC address. Similarly,
    /// IB only requires GUID allocation. Yet, since Mellanox devices support RoCE,
    /// NIC cards require allocation of GUID addresses. Similarly, since IB supports
    /// IP traffic HCA cards require allocation of MAC addresses.
    /// As both MAC addresses and GUID addresses are allocated together, there is a
    /// correlation between these 2 values. Unfortunately the translation from MAC
    /// address to GUID and vice-versa is inconsistent between different platforms and operating systems.
    /// To assure that this will not cause future issues, it is required that future
    /// devices will not rely on any conversion formulas between MAC and GUID values,
    /// and that these values will be explicitly stored in the device's nonvolatile memory.
    ///
    /// Assumption:
    /// redfish/v1/UpdateService/FirmwareInventory/DPU_SYS_IMAGE(Version)
    /// is identical to
    /// flint -d /dev/mst/mt*_pciconf0 q full (BASE GUID)
    ///
    /// Details:
    /// redfish/v1/UpdateService/FirmwareInventory/DPU_SYS_IMAGE
    /// is taken from /sys/class/infiniband/mlx*_<port>/sys_image_guid
    ///
    /// Example:
    /// DPU_SYS_IMAGE: a088:c203:0046:0c68
    /// Base GUID: a088c20300460c68
    /// Base MAC:  a088c2    460c68
    /// Note: 0300 in the middle looks as a constant for dpu
    ///
    /// redfish/v1/UpdateService/FirmwareInventory/DPU_SYS_IMAGE
    /// "Version": "a088:c203:0046:0c68"
    ///
    /// ibdev2netdev -v
    /// 0000:31:00.0 mlx5_0 (MT41692 - 900-9D3B6-00CV-AA0) BlueField-3 P-Series DPU 200GbE/NDR200 dual-port QSFP112,
    /// PCIe Gen5.0 x16 FHHL, Crypto Enabled, 32GB DDR5, BMC, Tall Bracket  fw 32.37.1306 port 1 (DOWN  ) ==> ens3np0 (Down)
    ///
    /// cat /sys/class/infiniband/mlx5_0/sys_image_guid
    /// a088:c203:0046:0c68
    ///
    /// ip link show ens3np0
    /// 6: ens3np0: <BROADCAST,MULTICAST> mtu 1500 qdisc noop state DOWN mode DEFAULT group default qlen 1000
    /// link/ether a0:88:c2:46:0c:68 brd ff:ff:ff:ff:ff:ff
    ///
    /// The method should be migrated to the DPU directly providing the
    /// MAC address: https://redmine.mellanox.com/issues/3749837
    fn find_host_pf_mac_address(&self) -> Result<MacAddress, String>;

    /// The host-facing PF MAC of a discovered DPU, or `None` if it can't be determined.
    fn get_host_pf_mac_address(&self) -> Option<MacAddress>;
}

impl ExploredEndpointExt for ExploredEndpoint {
    fn find_host_pf_mac_address(&self) -> Result<MacAddress, String> {
        // Base-MAC derivation has three paths, tried in order of trust:
        //   1. Primary  : any explored ComputerSystem base_mac (OEM Redfish BaseMAC).
        //   2. Legacy   : derived from UpdateService/FirmwareInventory/DPU_SYS_IMAGE.Version.
        //   3. BMC offset: derived from manager eth0 MAC minus per-platform offset.
        //
        // BF4 explicitly skips path 3. Its PF0 base MAC should be populated in
        // `systems[].base_mac` at exploration time from the NDF0 Redfish path.
        // BF3 keeps path 3 unchanged.

        // Path 1: explored computer-system base_mac.
        if let Some(system_mac) = self.report.systems.first().and_then(|s| s.base_mac) {
            return Ok(system_mac.to_mac());
        }

        // Path 2: legacy DPU_SYS_IMAGE derivation. Soft-fail so BF3 can still try
        // path 3 (BMC offset). BF4 should never use path 3.
        tracing::warn!("ComputerSystem doesn't have base_mac, trying DPU_SYS_IMAGE method");
        let legacy_err = match get_sys_image_version(self.report.service.as_ref())
            .and_then(get_base_mac_from_sys_image_version)
            .and_then(|legacy_mac| {
                sanitized_mac(&legacy_mac).map_err(|e| {
                    format!("Failed to build sanitized MAC from legacy/service MAC: {e} (source_mac: {legacy_mac})")
                })
            }) {
            Ok(mac) => return Ok(mac),
            Err(e) => e,
        };

        // BF4 should not use eth0 offset fallback.
        if is_bf4_dpu_report(&self.report) {
            tracing::warn!(
                "DPU_SYS_IMAGE derivation failed for BF4; expected PF0 base MAC from NDF0-patched systems[].base_mac, skipping BMC eth0 offset fallback"
            );
            return Err(legacy_err);
        }

        // Path 3: BMC manager eth0 MAC minus a per-platform offset.
        derive_base_mac_from_bmc_eth0(&self.report).ok_or(legacy_err)
    }

    fn get_host_pf_mac_address(&self) -> Option<MacAddress> {
        match self.find_host_pf_mac_address() {
            Ok(m) => Some(m),
            Err(error) => {
                tracing::error!(%error, dpu_bmc_ip_address = %self.address, "Failed to find base mac address for DPU");
                None
            }
        }
    }
}

/// Returns the DPU system-image version from the firmware inventory.
///
/// Returns an error if the inventory, system image, or version is missing.
pub fn get_sys_image_version(services: &[Service]) -> Result<&String, String> {
    let Some(service) = services.iter().find(|s| s.id == "FirmwareInventory") else {
        return Err("Missing FirmwareInventory".to_string());
    };

    let Some(image) = service
        .inventories
        .iter()
        .find(|inv| inv.id == "DPU_SYS_IMAGE")
    else {
        return Err("Missing DPU_SYS_IMAGE".to_string());
    };

    image
        .version
        .as_ref()
        .ok_or("Missing DPU_SYS_IMAGE version".to_string())
}

/// get_base_mac_from_sys_image_version returns a base MAC address
/// for a given sys image version/ See comments below about how the
/// DPU derives a MAC from a DPU_SYS_IMAGE, but ultimately, a
/// DPU_SYS_IMAGE of a088:c203:0046:0c68 means you just take out
/// chars 6-10, and you get a MAC of a0:88:c2:46:0c:68.
fn get_base_mac_from_sys_image_version(sys_image_version: &String) -> Result<String, String> {
    // The DPU_SYS_IMAGE is always 19 characters long. Well, until
    // it isn't, but for now, the DPU_SYS_IMAGE is 19 characters
    // long.
    if sys_image_version.len() != 19 {
        return Err(format!(
            "Invalid sys_image_version length: {} ({})",
            sys_image_version.len(),
            sys_image_version,
        ));
    }

    // First, strip out the colons, and make sure we're
    // left with 16 [what should be hex-friendly] characters.
    let mut base_mac = sys_image_version.replace(':', "");
    if base_mac.len() != 16 {
        return Err(format!(
            "Invalid base_mac length from sys_image_version after removing ':': {}",
            base_mac.len()
        ));
    }

    // And now drop range 6-10, leaving us with what
    // should be the 12 characters for the MAC address.
    base_mac.replace_range(6..10, "");

    Ok(base_mac)
}

fn is_bf4_dpu_report(report: &EndpointExplorationReport) -> bool {
    let has_bluefield_system = report.systems.first().is_some_and(is_bluefield_system);
    if !has_bluefield_system {
        return false;
    }

    // Use BF4-specific topology IDs instead of free-form model strings.
    // This is intentionally strict to avoid matching BF2/BF3 reports that may
    // also carry some *_0 naming in newer firmware.
    let has_bf4_chassis_and_nic = report.chassis.iter().any(|chassis| {
        chassis.id == "BlueField_0"
            && chassis
                .network_adapters
                .iter()
                .any(|adapter| adapter.id == "BlueField_NIC_0")
    });
    let has_bf4_bmc_manager_id = report
        .managers
        .iter()
        .any(|manager| manager.id == "BlueField_BMC_0");

    has_bf4_chassis_and_nic && has_bf4_bmc_manager_id
}

// The PF0 base MAC sits a fixed offset below the DPU BMC's eth0 MAC, within the
// contiguous MAC block allocated to the card. Per the BlueField-3 DPU Controller
// User Manual (§10.1, "DPU Controller Board Label"):
//   * host high-speed ports are `base + port_index`
//   * `DPU_BMC_MAC = OOB_MAC + 1`
// so the offset decomposes as `(OOB - base) + 1`.
//
// Measured on a real BF3 DPU (offset = 0x25 = 37):
//   DPU BMC eth0                              : 5c:25:73:9e:ac:eb
//   base (DPU_SYS_IMAGE 5c25:7303:009e:acc6)  : 5c:25:73:9e:ac:c6
// which implies OOB = bmc - 1 = ...ea and a host-reservation gap of
// (OOB - base) = 0x24 = 36, consistent with the manual's `BMC = OOB + 1`.
//
// The host-reservation gap is not published and could differ on other SKUs
// (e.g. 1- vs 2-port); revisit if a card of a different SKU mis-derives.
const BF3_ETH0_TO_BASE_MAC_OFFSET: u64 = 0x25; // measured: BlueField-3, see above

/// The per-platform offset to subtract from the BMC manager eth0 MAC to obtain
/// the DPU PF0 base MAC, or `None` for platforms we can't classify (we never guess).
fn bmc_eth0_to_base_mac_offset(report: &EndpointExplorationReport) -> Option<u64> {
    match report.identify_dpu()? {
        DpuModel::BlueField3 => Some(BF3_ETH0_TO_BASE_MAC_OFFSET),
        // BlueField-2 is not supported by the BMC eth0 offset fallback.
        DpuModel::BlueField2 | DpuModel::Unknown => None,
    }
}

/// Fallback-only (item #2 of issue #1076): derive the DPU PF0 base MAC from the
/// BMC manager eth0 MAC minus a platform-specific offset. Returns `None` if the
/// eth0 interface MAC is missing, locally-administered (pre-sync), the platform
/// is unknown, or the subtraction would underflow.
fn derive_base_mac_from_bmc_eth0(report: &EndpointExplorationReport) -> Option<MacAddress> {
    let offset = bmc_eth0_to_base_mac_offset(report)?;

    // Pick the eth0 interface specifically -- the OOB interface also lives in
    // the manager's ethernet_interfaces list.
    let bmc_eth0 = report
        .managers
        .iter()
        .flat_map(|m| m.ethernet_interfaces.iter())
        .find(|e| {
            e.id.as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case("eth0"))
        })
        .and_then(|e| e.mac_address)?;

    // A real NVIDIA BMC MAC is globally unique. A locally-administered MAC means
    // the BMC hasn't synced its burned-in address yet (transient post-boot
    // state) -- refuse to derive a base MAC from it rather than hand back a
    // plausible-but-wrong value.
    if is_locally_administered_mac(bmc_eth0) {
        tracing::warn!(
            bmc_eth0 = %bmc_eth0,
            "BMC eth0 MAC is locally-administered (pre-sync?); skipping offset derivation",
        );
        return None;
    }

    let derived = mac_to_u64(bmc_eth0).checked_sub(offset)?;
    let mac = u64_to_mac(derived);
    tracing::warn!(
        bmc_eth0 = %bmc_eth0,
        derived = %mac,
        "derived DPU base MAC from BMC eth0 via offset fallback",
    );
    Some(mac)
}

/// MAC address as a 48-bit big-endian integer (top two bytes of the u64 are zero).
pub(super) fn mac_to_u64(mac: MacAddress) -> u64 {
    mac.bytes()
        .iter()
        .fold(0u64, |acc, &byte| (acc << 8) | u64::from(byte))
}

/// Inverse of [`mac_to_u64`]; the high 16 bits are discarded.
pub(super) fn u64_to_mac(value: u64) -> MacAddress {
    let b = value.to_be_bytes();
    MacAddress::new([b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, Check, check_cases, check_values};
    use config_version::ConfigVersion;
    use model::site_explorer::PreingestionState;

    use super::*;
    use crate::test_support::{explored_endpoint, load_bf2_ep_report};

    #[test]
    fn mac_u64_roundtrip() {
        let mac: MacAddress = "a0:88:c2:46:0c:68".parse().unwrap();
        assert_eq!(mac_to_u64(mac), 0x0000_a088_c246_0c68);
        assert_eq!(u64_to_mac(mac_to_u64(mac)), mac);
    }

    #[test]
    fn u64_to_mac_discards_high_bits() {
        // High 16 bits set must not leak into the MAC bytes.
        assert_eq!(
            u64_to_mac(0xffff_a088_c246_0c68),
            "a0:88:c2:46:0c:68".parse().unwrap()
        );
    }

    #[test]
    fn bf3_offset_derives_measured_base_mac() {
        // Real BF3 DPU measurement (see BF3_ETH0_TO_BASE_MAC_OFFSET):
        // BMC eth0 - offset must yield the DPU_SYS_IMAGE-derived base MAC.
        let bmc_eth0: MacAddress = "5c:25:73:9e:ac:eb".parse().unwrap();
        let base: MacAddress = "5c:25:73:9e:ac:c6".parse().unwrap();
        let derived = u64_to_mac(mac_to_u64(bmc_eth0) - BF3_ETH0_TO_BASE_MAC_OFFSET);
        assert_eq!(derived, base);
        // Cross-check the documented BMC = OOB + 1 relationship.
        let oob = u64_to_mac(mac_to_u64(bmc_eth0) - 1);
        assert_eq!(oob, "5c:25:73:9e:ac:ea".parse().unwrap());
    }

    const BF3_MODEL_VARIANTS: [&str; 2] = ["NVIDIA BlueField 3 DPU", "BlueField-3 DPU"];

    // Minimal BlueField-3 report with a single manager eth0 interface carrying
    // `eth0_mac`. Classifies as BF3 (system id "Bluefield" + Card1 BF3 chassis).
    fn bf3_report_with_eth0(model: &str, eth0_mac: &str) -> EndpointExplorationReport {
        use model::site_explorer::{Chassis, ComputerSystem, EthernetInterface, Manager};
        EndpointExplorationReport {
            systems: vec![ComputerSystem {
                id: "Bluefield".to_string(),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                id: "Card1".to_string(),
                model: Some(model.to_string()),
                ..Default::default()
            }],
            managers: vec![Manager {
                id: "Bluefield_BMC".to_string(),
                ipmi_port: None,
                ethernet_interfaces: vec![EthernetInterface {
                    id: Some("eth0".to_string()),
                    mac_address: Some(eth0_mac.parse().unwrap()),
                    ..Default::default()
                }],
            }],
            ..Default::default()
        }
    }

    fn bf4_report_with_zero_suffix_ids(system_id: &str) -> EndpointExplorationReport {
        use model::site_explorer::{Chassis, ComputerSystem, Manager, NetworkAdapter};
        EndpointExplorationReport {
            systems: vec![ComputerSystem {
                id: system_id.to_string(),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                id: "BlueField_0".to_string(),
                network_adapters: vec![NetworkAdapter {
                    id: "BlueField_NIC_0".to_string(),
                    ..Default::default()
                }],
                model: None,
                ..Default::default()
            }],
            managers: vec![Manager {
                id: "BlueField_BMC_0".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn is_bf4_dpu_report_detects_zero_suffix_ids_without_model_string() {
        for system_id in ["Bluefield", "BlueField_0"] {
            let report = bf4_report_with_zero_suffix_ids(system_id);
            assert!(
                is_bf4_dpu_report(&report),
                "expected BF4 detection for system id {system_id}"
            );
        }
    }

    #[test]
    fn is_bf4_dpu_report_rejects_zero_suffix_ids_without_bf4_nic_topology() {
        use model::site_explorer::{Chassis, ComputerSystem, Manager};
        let report = EndpointExplorationReport {
            systems: vec![ComputerSystem {
                id: "Bluefield".to_string(),
                ..Default::default()
            }],
            chassis: vec![Chassis {
                id: "BlueField_0".to_string(),
                // Missing BlueField_NIC_0 adapter.
                ..Default::default()
            }],
            managers: vec![Manager {
                id: "BlueField_BMC_0".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!is_bf4_dpu_report(&report));
    }

    #[test]
    fn is_bf4_dpu_report_does_not_match_bf3_shape() {
        check_values(
            BF3_MODEL_VARIANTS.map(|model| Check {
                scenario: model,
                input: model,
                expect: false,
            }),
            |model| is_bf4_dpu_report(&bf3_report_with_eth0(model, "5c:25:73:9e:ac:eb")),
        );
    }

    #[test]
    fn bmc_eth0_offset_skips_locally_administered_mac() {
        check_values(
            [
                Check {
                    scenario: "space-separated BF3 model with pre-sync eth0",
                    input: (BF3_MODEL_VARIANTS[0], "9a:72:d5:07:ae:7e"),
                    expect: None,
                },
                Check {
                    scenario: "hyphenated BF3 model with pre-sync eth0",
                    input: (BF3_MODEL_VARIANTS[1], "9a:72:d5:07:ae:7e"),
                    expect: None,
                },
                Check {
                    scenario: "space-separated BF3 model with synced eth0",
                    input: (BF3_MODEL_VARIANTS[0], "5c:25:73:9e:ac:eb"),
                    expect: Some("5c:25:73:9e:ac:c6".parse().unwrap()),
                },
                Check {
                    scenario: "hyphenated BF3 model with synced eth0",
                    input: (BF3_MODEL_VARIANTS[1], "5c:25:73:9e:ac:eb"),
                    expect: Some("5c:25:73:9e:ac:c6".parse().unwrap()),
                },
            ],
            |(model, eth0_mac)| {
                let report = bf3_report_with_eth0(model, eth0_mac);
                assert_eq!(bmc_eth0_to_base_mac_offset(&report), Some(0x25));
                derive_base_mac_from_bmc_eth0(&report)
            },
        );
    }

    #[test]
    fn bmc_eth0_offset_fallback_unsupported_for_bf2() {
        // BlueField-2 is intentionally not supported by the BMC eth0 offset
        // fallback, so derivation must return None even when an eth0 MAC exists.
        let mut report = load_bf2_ep_report();
        for s in report.systems.iter_mut() {
            s.base_mac = None;
        }
        assert!(bmc_eth0_to_base_mac_offset(&report).is_none());
        assert!(derive_base_mac_from_bmc_eth0(&report).is_none());
    }

    #[test]
    fn test_find_host_pf_mac_address() {
        // A freshly-loaded BF2 endpoint; each case starts from one of these and
        // perturbs the firmware inventory the legacy MAC lookup reads from.
        let endpoint = || ExploredEndpoint {
            address: "10.217.132.202".parse().unwrap(),
            report: load_bf2_ep_report(),
            report_version: ConfigVersion::initial(),
            preingestion_state: PreingestionState::Initial,
            waiting_for_explorer_refresh: false,
            exploration_requested: false,
            last_redfish_bmc_reset: None,
            last_ipmitool_bmc_reset: None,
            last_redfish_reboot: None,
            last_redfish_powercycle: None,
            pause_ingestion_and_poweron: false,
            pause_remediation: false,
            boot_interface_mac: None,
            boot_interface_id: None,
        };

        // Override the `DPU_SYS_IMAGE` firmware version the legacy path parses.
        let with_sys_image = |version: &str| {
            let mut ep = endpoint();
            let inv = ep
                .report
                .service
                .iter_mut()
                .find(|s| s.id == "FirmwareInventory")
                .unwrap()
                .inventories
                .iter_mut()
                .find(|inv| inv.id == "DPU_SYS_IMAGE")
                .unwrap();
            inv.version = Some(version.to_string());
            ep
        };

        let bf3_with_empty_sys_image = |model| {
            let mut ep = explored_endpoint(bf3_report_with_eth0(model, "5c:25:73:9e:ac:eb"));
            ep.report.service = with_sys_image("").report.service;
            ep
        };

        // Drop the firmware-inventory entry whose `id` matches `inventory_id`.
        let without_inventory = |inventory_id: &str| {
            let mut ep = endpoint();
            ep.report
                .service
                .iter_mut()
                .find(|s| s.id == "FirmwareInventory")
                .unwrap()
                .inventories
                .retain(|inv| inv.id != inventory_id);
            ep
        };

        // Drop the whole `FirmwareInventory` service.
        let without_firmware_inventory = || {
            let mut ep = endpoint();
            ep.report.service.retain(|s| s.id != "FirmwareInventory");
            ep
        };

        check_cases(
            [
                Case {
                    scenario: "report base_mac wins before legacy DPU_SYS_IMAGE path",
                    input: {
                        let mut ep = without_firmware_inventory();
                        ep.report.systems[0].base_mac =
                            Some("f4:20:4d:49:53:b4".parse::<MacAddress>().unwrap().into());
                        ep
                    },
                    expect: Yields("f4:20:4d:49:53:b4".parse().unwrap()),
                },
                Case {
                    scenario: "legacy sys-image MAC, sanitized",
                    input: endpoint(),
                    expect: Yields("B8:3F:D2:90:95:F4".parse().unwrap()),
                },
                Case {
                    scenario: "space-separated BF3 model falls back to BMC eth0",
                    input: bf3_with_empty_sys_image(BF3_MODEL_VARIANTS[0]),
                    expect: Yields("5c:25:73:9e:ac:c6".parse().unwrap()),
                },
                Case {
                    scenario: "hyphenated BF3 model falls back to BMC eth0",
                    input: bf3_with_empty_sys_image(BF3_MODEL_VARIANTS[1]),
                    expect: Yields("5c:25:73:9e:ac:c6".parse().unwrap()),
                },
                Case {
                    scenario: "legacy sys-image MAC fails sanitization",
                    input: with_sys_image("b83f:d203:0090:95fz"),
                    expect: FailsWith("Failed to build sanitized MAC from legacy/service MAC: invalid stripped MAC length: 11 (input: b83fd29095fz, output: b83fd29095f) (source_mac: b83fd29095fz)".to_string()),
                },
                Case {
                    scenario: "legacy sys-image is too short",
                    input: with_sys_image("abc"),
                    expect: FailsWith("Invalid sys_image_version length: 3 (abc)".to_string()),
                },
                Case {
                    scenario: "no DPU_SYS_IMAGE inventory",
                    input: without_inventory("DPU_SYS_IMAGE"),
                    expect: FailsWith("Missing DPU_SYS_IMAGE".to_string()),
                },
                Case {
                    scenario: "no FirmwareInventory service",
                    input: without_firmware_inventory(),
                    expect: FailsWith("Missing FirmwareInventory".to_string()),
                },
            ],
            |ep| ep.find_host_pf_mac_address(),
        );
    }
}
