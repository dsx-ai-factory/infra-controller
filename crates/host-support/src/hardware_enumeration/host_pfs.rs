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

//! Collects factual host-PF inventory without embedding carbide-api's controller policy.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use model::hardware_info::{DpuData, DpuHostPf};
use model::pci::PciBdfOrderingKey;

use super::HardwareEnumerationError;

/// Collects each semantic host PF with its own MAC so Linux interface naming cannot alter selection.
pub(super) fn collect(sysfs_net: &Path) -> Result<Vec<DpuHostPf>, HardwareEnumerationError> {
    let read_error = |path, source| HardwareEnumerationError::HostPfSysfs { path, source };
    let entries =
        fs::read_dir(sysfs_net).map_err(|source| read_error(sysfs_net.to_path_buf(), source))?;
    let mut inventory = DpuData::default();
    for entry in entries {
        let entry = entry.map_err(|source| read_error(sysfs_net.to_path_buf(), source))?;
        let path = entry.path();
        let physical_port_path = path.join("phys_port_name");
        let physical_port_name = match fs::read_to_string(&physical_port_path) {
            Ok(name) => name.trim().to_string(),
            // Ordinary netdevs may omit or not implement this optional sysfs attribute.
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::Unsupported | ErrorKind::InvalidInput
                ) =>
            {
                continue;
            }
            Err(source) => return Err(read_error(physical_port_path, source)),
        };
        let Some((pci_controller_id, pf_id)) =
            DpuHostPf::parse_physical_port_name(&physical_port_name)
        else {
            continue;
        };

        // Read the address from the same netdev as its semantic controller/PF name.
        let address_path = path.join("address");
        let address =
            fs::read_to_string(&address_path).map_err(|source| read_error(address_path, source))?;
        let mac_address = address
            .trim()
            .parse()
            .map_err(|_| HardwareEnumerationError::InvalidMacAddress(address.trim().to_string()))?;

        // Optional PCI metadata cannot choose or replace the boot MAC.
        let pci_address = match fs::canonicalize(path.join("device")) {
            Ok(device) => device
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|address| PciBdfOrderingKey::from_bdf(address).is_ok())
                .map(str::to_string),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(source) => return Err(read_error(path.join("device"), source)),
        };
        inventory.host_pfs.push(DpuHostPf {
            pci_controller_id,
            pf_id,
            mac_address,
            physical_port_name,
            netdev_name: entry.file_name().into_string().ok(),
            pci_address,
        });
    }

    // Reject duplicate identities before sorting so enumeration order cannot hide ambiguity.
    inventory.validate_host_pfs()?;
    inventory.host_pfs.sort_by(|left, right| {
        (left.pci_controller_id, left.pf_id, &left.netdev_name).cmp(&(
            right.pci_controller_id,
            right.pf_id,
            &right.netdev_name,
        ))
    });
    Ok(inventory.host_pfs)
}

#[cfg(test)]
mod tests {
    use model::hardware_info::DpuHostPfError;

    use super::*;

    /// Creates a netdev's sysfs files so collection tests bind semantic identity to the same address.
    fn netdev(root: &Path, name: &str, physical_port_name: &str, address: &str) {
        // Keep both facts beneath one directory, as the kernel exposes them.
        let path = root.join(name);
        fs::create_dir_all(&path).expect("create netdev directory");
        fs::write(path.join("phys_port_name"), physical_port_name)
            .expect("write physical-port name");
        fs::write(path.join("address"), address).expect("write netdev MAC");
    }

    /// Collects every controller with its matching MAC in deterministic order despite unrelated netdev names.
    #[test]
    fn collects_all_controllers_and_matching_addresses() {
        // Interface names deliberately disagree with controller ordering.
        let sysfs = tempfile::tempdir().expect("temporary sysfs");
        netdev(
            sysfs.path(),
            "first-netdev",
            "c2pf0\n",
            "96:ab:d0:3e:5f:a6\n",
        );
        netdev(
            sysfs.path(),
            "last-netdev",
            "c1pf12\n",
            "96:ab:d0:3e:5f:a8\n",
        );
        netdev(sysfs.path(), "uplink", "p0\n", "96:ab:d0:3e:5f:aa\n");
        fs::create_dir(sysfs.path().join("loopback")).expect("netdev without physical-port name");

        // Observe all controllers without supplying an API controller selection.
        let inventory = collect(sysfs.path()).expect("valid host-PF inventory");

        // Sorting must retain each address's association with its own semantic name.
        let observed = inventory
            .iter()
            .map(|entry| {
                (
                    entry.pci_controller_id,
                    entry.pf_id,
                    entry.mac_address.bytes(),
                    entry.netdev_name.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                (
                    1,
                    12,
                    [0x96, 0xab, 0xd0, 0x3e, 0x5f, 0xa8],
                    Some("last-netdev")
                ),
                (
                    2,
                    0,
                    [0x96, 0xab, 0xd0, 0x3e, 0x5f, 0xa6],
                    Some("first-netdev")
                ),
            ]
        );
    }

    /// Rejects two netdevs claiming one host PF so registration cannot depend on directory ordering.
    #[test]
    fn rejects_duplicate_controller_and_pf() {
        // Distinct interface names and addresses still claim the same hardware identity.
        let sysfs = tempfile::tempdir().expect("temporary sysfs");
        netdev(sysfs.path(), "one", "c1pf0", "96:ab:d0:3e:5f:a6");
        netdev(sysfs.path(), "two", "c1pf0", "96:ab:d0:3e:5f:a8");

        // Collect the complete observation before deciding whether it is unambiguous.
        let result = collect(sysfs.path());

        // Preserve the typed duplicate reason for actionable registration failures.
        assert!(matches!(
            result,
            Err(HardwareEnumerationError::DpuHostPf(
                DpuHostPfError::DuplicateIdentity {
                    controller: 1,
                    pf: 0
                }
            ))
        ));
    }
}
