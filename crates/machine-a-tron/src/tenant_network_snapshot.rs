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
//! Opt-in, credential-free snapshots for the external tenant packet test.
use std::path::{Path, PathBuf};

use carbide_uuid::machine::MachineId;
use rpc::forge::ManagedHostNetworkConfigResponse;

#[derive(Default)]
pub(super) struct TenantNetworkSnapshot {
    path: Option<PathBuf>,
}

impl TenantNetworkSnapshot {
    pub(super) fn prepare_directory(directory: Option<&Path>) -> std::io::Result<()> {
        let Some(directory) = directory else {
            return Ok(());
        };
        std::fs::create_dir_all(directory)?;
        drop(tempfile::NamedTempFile::new_in(directory)?);
        let entries = std::fs::read_dir(directory)?;
        // A restarted process must not advertise observations from its predecessor.
        for entry in entries {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.parse::<MachineId>().is_ok())
            {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    pub(super) fn update(
        &mut self,
        directory: Option<&Path>,
        machine_id: MachineId,
        config: &ManagedHostNetworkConfigResponse,
    ) -> std::io::Result<()> {
        let Some(directory) = directory.filter(|_| {
            config.is_primary_dpu
                && !config.use_admin_network
                && config.instance_id.is_some()
                && !config.tenant_interfaces.is_empty()
        }) else {
            return self.clear();
        };
        std::fs::create_dir_all(directory)?;
        let path = directory.join(format!("{machine_id}.json"));
        if self.path.as_ref().is_some_and(|previous| previous != &path) {
            self.clear()?;
        }
        // Whitelist network fields: the full response also contains credentials.
        let interfaces: Vec<_> = config
            .tenant_interfaces
            .iter()
            .map(|interface| {
                serde_json::json!({
                    "function_type": interface.function_type,
                    "is_l2_segment": interface.is_l2_segment,
                    "vpc_vni": interface.vpc_vni,
                    "addresses": interface.addresses,
                    "has_network_security_group": interface.network_security_group.is_some(),
                })
            })
            .collect();
        let snapshot = serde_json::json!({
            "schema_version": 1,
            "updated_at": chrono::Utc::now(),
            "dpu_id": machine_id,
            "instance_id": config.instance_id,
            "host_interface_id": config.host_interface_id,
            "managed_host_config_version": config.managed_host_config_version,
            "instance_network_config_version": config.instance_network_config_version,
            "network_virtualization_type": config.network_virtualization_type,
            "tenant_interfaces": interfaces,
        });
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut file, &snapshot)?;
        // Readers see either complete revision, never a partially written JSON document.
        file.persist(&path).map_err(|error| error.error)?;
        self.path = Some(path);
        Ok(())
    }

    fn clear(&mut self) -> std::io::Result<()> {
        if let Some(path) = &self.path {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            self.path = None;
        }
        Ok(())
    }
}

impl Drop for TenantNetworkSnapshot {
    fn drop(&mut self) {
        if let Err(error) = self.clear() {
            tracing::warn!(%error, path = ?self.path, "Failed to remove tenant network snapshot");
        }
    }
}

#[cfg(test)]
mod tests {
    use rpc::forge::{AddressFamily, FlatInterfaceConfig, InterfaceAddressConfig};
    use serde_json::Value;

    use super::*;

    fn tenant_config() -> ManagedHostNetworkConfigResponse {
        ManagedHostNetworkConfigResponse {
            is_primary_dpu: true,
            instance_id: Some("00812118-8c3e-44cb-8dc5-fd9250ddc8f8".parse().unwrap()),
            host_interface_id: Some("test-host-interface".to_string()),
            managed_host_config_version: "version-1".to_string(),
            bgp_leaf_session_password: Some("must-not-export".to_string()),
            tenant_interfaces: vec![FlatInterfaceConfig {
                addresses: vec![InterfaceAddressConfig {
                    address_family: AddressFamily::V6 as i32,
                    ip: "fd00:84:100::1".to_string(),
                    prefix: "fd00:84:100::/127".to_string(),
                    interface_prefix: "fd00:84:100::1/128".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn dpu_id() -> MachineId {
        "fm100dsq7h9eabr4qrfh88vipv7il5sbpfgfq6lhb30n36offacbmusfb50"
            .parse()
            .unwrap()
    }

    #[test]
    fn snapshot_follows_tenant_configuration_and_lifetime() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{}.json", dpu_id()));
        let mut snapshot = TenantNetworkSnapshot::default();
        let mut config = tenant_config();
        snapshot
            .update(Some(directory.path()), dpu_id(), &config)
            .unwrap();
        let read = || serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap();
        let first = read();
        assert_eq!(first["schema_version"], 1);
        assert_eq!(
            first["tenant_interfaces"][0]["addresses"][0]["ip"],
            "fd00:84:100::1"
        );
        assert!(!first.to_string().contains("must-not-export"));
        assert!(first["updated_at"].as_str().is_some());

        config.managed_host_config_version = "version-2".to_string();
        config.tenant_interfaces[0].addresses[0].ip = "fd00:84:100::3".to_string();
        snapshot
            .update(Some(directory.path()), dpu_id(), &config)
            .unwrap();
        let second = read();
        assert_eq!(second["managed_host_config_version"], "version-2");
        assert_eq!(
            second["tenant_interfaces"][0]["addresses"][0]["ip"],
            "fd00:84:100::3"
        );

        config.use_admin_network = true;
        snapshot
            .update(Some(directory.path()), dpu_id(), &config)
            .unwrap();
        assert!(!path.exists(), "release must remove the tenant snapshot");
        config.use_admin_network = false;
        snapshot
            .update(Some(directory.path()), dpu_id(), &config)
            .unwrap();
        drop(snapshot);
        assert!(!path.exists(), "stopping the DPU must remove the snapshot");
    }

    #[test]
    fn startup_removes_only_previous_machine_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{}.json", dpu_id()));
        std::fs::write(&path, "previous process snapshot").unwrap();
        let unrelated = directory.path().join("notes.json");
        std::fs::write(&unrelated, "keep").unwrap();
        TenantNetworkSnapshot::prepare_directory(None).unwrap();
        assert!(path.exists(), "disabled export must leave files untouched");
        TenantNetworkSnapshot::prepare_directory(Some(directory.path())).unwrap();
        assert!(
            !path.exists(),
            "startup must retire snapshots before restoring devices"
        );
        assert!(unrelated.exists());
        let new_directory = directory.path().join("absent");
        TenantNetworkSnapshot::prepare_directory(Some(&new_directory)).unwrap();
        assert!(
            new_directory.is_dir(),
            "startup must create the configured directory"
        );
        assert!(TenantNetworkSnapshot::prepare_directory(Some(&unrelated)).is_err());
    }

    #[test]
    fn disabled_and_secondary_dpus_do_not_export() {
        let directory = tempfile::tempdir().unwrap();
        let mut snapshot = TenantNetworkSnapshot::default();
        let mut config = tenant_config();
        snapshot.update(None, dpu_id(), &config).unwrap();
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        config.is_primary_dpu = false;
        snapshot
            .update(Some(directory.path()), dpu_id(), &config)
            .unwrap();
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
