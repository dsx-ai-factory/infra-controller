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
use std::ffi::OsString;
use std::fs::{self, DirEntry};
use std::io::Result;

const SYSFS_NET_BASE: &str = "/sys/class/net";

pub(super) fn get_net_devices() -> Result<Vec<SysfsNetDevice>> {
    let net_device_entries = fs::read_dir(SYSFS_NET_BASE)?;
    net_device_entries
        .map(|entry| entry.map(SysfsNetDevice::from))
        .collect()
}

pub(super) struct SysfsNetDevice {
    dir_entry: DirEntry,
}

impl SysfsNetDevice {
    pub(super) fn is_pci_device(&self) -> Result<bool> {
        const PCI_PREFIX: &str = "../../devices/pci";
        let link_target = fs::read_link(self.dir_entry.path());
        link_target.map(|link_target| {
            link_target
                .as_os_str()
                .as_encoded_bytes()
                .starts_with(PCI_PREFIX.as_bytes())
        })
    }

    pub(super) fn entry_name(&self) -> OsString {
        self.dir_entry.file_name()
    }
}

impl From<DirEntry> for SysfsNetDevice {
    fn from(dir_entry: DirEntry) -> Self {
        SysfsNetDevice { dir_entry }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn test_sysfs_net_devices() {
        let net_devices = get_net_devices().expect("Couldn't get sysfs network devices");
        assert!(!net_devices.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_lo_is_not_pci_device() {
        let net_devices = get_net_devices().expect("Couldn't get sysfs network devices");
        let lo_device = net_devices
            .iter()
            .find(|d| d.entry_name().as_encoded_bytes() == "lo".as_bytes())
            .expect("Couldn't find lo network device");
        let lo_is_pci_device = lo_device.is_pci_device().unwrap();
        assert!(!lo_is_pci_device);
    }
}
