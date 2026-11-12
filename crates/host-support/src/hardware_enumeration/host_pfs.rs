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

//! Collect host-visible PF MACs directly from the DPU's netdev attributes.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use eyre::{WrapErr, bail};
use mac_address::MacAddress;
use rpc::machine_discovery::{DiscoveryInfo, DpuHostPf};

/// Refreshes BF4 PF facts for registration while leaving other DPU generations independent of PF sysfs access.
pub fn refresh_host_pfs(
    hardware_info: &mut DiscoveryInfo,
    net_directory: &Path,
) -> eyre::Result<()> {
    let Some(dpu) = hardware_info.dpu_info.as_mut() else {
        bail!("missing DPU info during registration");
    };

    // Use Core's VPD/DMI classification so BF2/BF3 registration never depends on PF collection.
    if carbide_utils::is_bf4_dpu(
        &dpu.part_number,
        hardware_info
            .dmi_data
            .as_ref()
            .map_or("", |dmi| dmi.product_name.as_str()),
    ) {
        dpu.host_pfs = enumerate_host_pfs(net_directory)?;
    }
    Ok(())
}

/// Collects same-netdev host PF identities and MACs afresh for registration retries.
///
/// `net_directory` names the DPU's `/sys/class/net` directory. Unrelated port names and
/// unsupported physical-port attributes are skipped. Read failures, invalid MACs and duplicate
/// controller/PF identities fail the attempt. Results are ordered by controller and PF.
pub fn enumerate_host_pfs(net_directory: &Path) -> eyre::Result<Vec<DpuHostPf>> {
    let mut host_pfs = BTreeMap::new();

    // Read physical-port identities independently of netdev names and enumeration order.
    for entry in fs::read_dir(net_directory).wrap_err_with(|| {
        format!(
            "could not enumerate DPU netdevs in {}",
            net_directory.display()
        )
    })? {
        let netdev = entry?.path();
        let port_name = match fs::read_to_string(netdev.join("phys_port_name")) {
            Ok(name) => name,
            // Virtual netdevs can omit this attribute or report that it is unsupported.
            Err(error)
                if error.kind() == ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::EOPNOTSUPP) =>
            {
                continue;
            }
            Err(error) => {
                return Err(error).wrap_err_with(|| {
                    format!("could not read {}/phys_port_name", netdev.display())
                });
            }
        };
        let Some((pci_controller_id, pf_id)) =
            parse_host_pf_port_name(port_name.trim_end_matches('\n'))
        else {
            continue;
        };

        // Read the address only after identifying this exact netdev as a host PF.
        let address = fs::read_to_string(netdev.join("address"))
            .wrap_err_with(|| format!("could not read {}/address", netdev.display()))?;
        let mac_address: MacAddress = address
            .trim_end_matches('\n')
            .parse()
            .wrap_err_with(|| format!("invalid host PF MAC in {}/address", netdev.display()))?;
        let octets = mac_address.bytes();
        if octets == [0; 6] || octets[0] & 1 != 0 {
            bail!(
                "invalid host PF MAC {mac_address} in {}/address",
                netdev.display()
            );
        }
        if host_pfs
            .insert(
                (pci_controller_id, pf_id),
                DpuHostPf {
                    pci_controller_id: u32::from(pci_controller_id),
                    pf_id: u32::from(pf_id),
                    mac_address: mac_address.to_string(),
                },
            )
            .is_some()
        {
            bail!("duplicate host PF identity c{pci_controller_id}pf{pf_id}");
        }
    }
    Ok(host_pfs.into_values().collect())
}

/// Recognizes complete decimal host PF names so VFs and uplinks cannot supply a host MAC.
fn parse_host_pf_port_name(name: &str) -> Option<(u8, u8)> {
    let (controller, pf) = name.strip_prefix('c')?.split_once("pf")?;
    if controller.is_empty()
        || pf.is_empty()
        || !controller.bytes().all(|digit| digit.is_ascii_digit())
        || !pf.bytes().all(|digit| digit.is_ascii_digit())
    {
        return None;
    }
    Some((controller.parse().ok()?, pf.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// Requires PF access for BF4 VPD or DMI evidence while BF2/BF3 register without touching unavailable sysfs.
    #[test]
    fn refresh_requires_pf_access_only_for_bf4() {
        use carbide_test_support::Outcome::{Fails, Yields};
        use carbide_test_support::scenarios;
        use rpc::machine_discovery::{DmiData, DpuData};

        // An absent directory makes any attempted PF collection observable without depending on host devices.
        let root = tempfile::tempdir().expect("temporary sysfs");
        let unavailable = root.path().join("unavailable");
        scenarios!(
            run = |(part_number, product_name): (&str, &str)| {
                let mut hardware_info = DiscoveryInfo {
                    dpu_info: Some(DpuData {
                        part_number: part_number.to_string(), // VPD can identify BF4 independently of DMI.
                        ..Default::default()
                    }),
                    dmi_data: Some(DmiData {
                        product_name: product_name.to_string(), // DMI must retain BF4 acquisition when VPD is absent.
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                // Exercise the registration refresh operation, including its acquisition-failure policy.
                refresh_host_pfs(&mut hardware_info, &unavailable).map_err(drop)
            };
            "legacy registration" {
                // BF2 must not acquire a new sysfs dependency.
                ("MBF2H516A-CEEO", "BlueField SoC") => Yields(()),
                // BF3 keeps its factory-MAC registration path even when PF enumeration is unavailable.
                ("900-9D3B6-00CV-AA0", "BlueField-3 SmartNIC Main Card") => Yields(()),
            }
            "required BF4 acquisition" {
                // A BF4 VPD match cannot ignore collection failure just because DMI is generic.
                ("900-9D4B4-CWAA-TSA", "BlueField SoC") => Fails,
                // Missing VPD cannot bypass a BF4 DMI match.
                ("", "BlueField-4") => Fails,
            }
        );
    }

    /// Verifies complete decimal PF names resolve while other ports and overflowing IDs are excluded.
    /// Rejecting suffixes prevents a VF or SF address from becoming the host's PF MAC.
    #[test]
    fn host_pf_port_names_require_complete_decimal_ids() {
        value_scenarios!(
            run = parse_host_pf_port_name;
            "host PF identity" {
                // Resolve a PF independently of the Linux interface name.
                "c1pf0" => Some((1, 0)),
                // Controller zero remains usable when selected by API configuration.
                "c0pf0" => Some((0, 0)),
                // Accept decimal IDs through the supported hardware limit.
                "c255pf255" => Some((255, 255)),
            }
            "unrelated ports" {
                // An uplink is not a host PF.
                "p0" => None,
                // A VF has its own address and cannot identify the PF.
                "c1pf0vf0" => None,
            }
            "malformed identities" {
                // Require a controller instead of guessing one from a PF name.
                "pf0" => None,
                // An empty controller must not be interpreted as controller zero.
                "cpf0" => None,
                // Require both decimal components.
                "c1pf" => None,
                // Numeric signs are outside the sysfs physical-port grammar.
                "c+1pf0" => None,
                // Reject controller overflow without truncation or a panic.
                "c256pf0" => None,
                // Check PF overflow independently of the controller number.
                "c1pf256" => None,
            }
        );
    }

    /// Constructs one sysfs-like netdev so tests control physical identity separately from its name.
    fn netdev(root: &Path, name: &str, port_name: &str, address: &str) {
        // Keep both attributes on the same netdev to model the kernel's MAC association.
        let directory = root.join(name);
        fs::create_dir(&directory).expect("create netdev");
        fs::write(directory.join("phys_port_name"), port_name).expect("write physical port name");
        fs::write(directory.join("address"), address).expect("write MAC address");
    }

    /// Verifies collection uses the matching netdev and stable numeric order, then observes a delayed PF.
    /// A partial observation must not freeze subsequent acquisition attempts.
    #[test]
    fn host_pfs_use_same_netdev_and_refresh_partial_inventory() {
        // Make interface names disagree with controller order and add an unrelated invalid address.
        let root = tempfile::tempdir().expect("temporary sysfs");
        netdev(root.path(), "first", "c2pf0\n", "02:00:00:00:00:20\n");
        netdev(root.path(), "uplink", "p0\n", "not-a-host-MAC");
        let initial_inventory = enumerate_host_pfs(root.path()).expect("first acquisition");
        assert_eq!(initial_inventory.len(), 1);

        // Collect again after a second PF becomes visible.
        netdev(root.path(), "last", "c1pf0\n", "02:00:00:00:00:10\n");
        let refreshed_inventory = enumerate_host_pfs(root.path()).expect("fresh acquisition");

        // Compare identities and addresses to detect ordering guesses and stale reuse.
        let observed = refreshed_inventory
            .iter()
            .map(|pf| (pf.pci_controller_id, pf.pf_id, pf.mac_address.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![(1, 0, "02:00:00:00:00:10"), (2, 0, "02:00:00:00:00:20")]
        );
    }

    /// Skips unrelated netdevs lacking physical-port identity while failing acquisition when an identified PF's address is unavailable, preventing silent partial registration.
    #[test]
    fn host_pf_attribute_failures_respect_physical_identity() {
        // An unrelated virtual netdev may omit both attributes beside a valid host PF.
        let root = tempfile::tempdir().expect("temporary sysfs");
        fs::create_dir(root.path().join("virtual")).expect("unrelated netdev");
        netdev(root.path(), "host", "c1pf0", "02:00:00:00:00:10");
        let observed =
            enumerate_host_pfs(root.path()).expect("missing unrelated identity is skipped");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].mac_address, "02:00:00:00:00:10");

        for (scenario, unreadable) in [
            // Missing address after recognizing a PF must fail instead of silently dropping its fact.
            ("missing PF address", false),
            // A directory causes an actual read failure even when tests run with elevated filesystem permissions.
            ("unreadable PF address", true),
        ] {
            // Keep the recognized PF identity fixed while varying the address read failure.
            let address = root.path().join("host/address");
            fs::remove_file(&address).expect("remove address");
            if unreadable {
                fs::create_dir(&address).expect("unreadable address attribute");
            }
            let error =
                enumerate_host_pfs(root.path()).expect_err("recognized PF address is required");
            assert!(
                error.to_string().contains("host/address"),
                "{scenario}: {error}"
            );
            // Restore the readable attribute before the next independent failure case.
            if unreadable {
                fs::remove_dir(&address).expect("remove unreadable attribute");
            }
            fs::write(&address, "02:00:00:00:00:10").expect("restore address");
        }
    }

    /// Verifies duplicate controller/PF identities fail even with equal MACs, avoiding ambiguous binding.
    #[test]
    fn duplicate_host_pf_identities_are_rejected() {
        // Equal MACs do not make two netdevs claiming the same hardware identity unique.
        let root = tempfile::tempdir().expect("temporary sysfs");
        netdev(root.path(), "one", "c1pf0", "02:00:00:00:00:10");
        netdev(root.path(), "two", "c1pf0", "02:00:00:00:00:10");

        // Fail the attempt instead of depending on directory iteration order.
        assert!(
            enumerate_host_pfs(root.path())
                .expect_err("duplicate identity")
                .to_string()
                .contains("duplicate host PF identity c1pf0")
        );
    }

    /// Verifies unusable host PF addresses fail acquisition so registration cannot report invalid facts.
    #[test]
    fn invalid_host_pf_macs_are_rejected() {
        use carbide_test_support::Outcome::Fails;
        use carbide_test_support::scenarios;

        scenarios!(
            run = |address: &str| {
                // Keep the physical identity fixed to isolate each invalid-address contract.
                let root = tempfile::tempdir().expect("temporary sysfs");
                netdev(root.path(), "host", "c1pf0", address);

                // Collect the PF to exercise address parsing and host-MAC validation together.
                enumerate_host_pfs(root.path()).map(|_| ()).map_err(drop)
            };
            "unusable host MACs" {
                // Malformed text cannot supply a MAC.
                "not-a-MAC" => Fails,
                // An unset address is not a usable host identity.
                "00:00:00:00:00:00" => Fails,
                // Group addresses cannot identify one host PF.
                "01:00:00:00:00:01" => Fails,
            }
        );
    }
}
