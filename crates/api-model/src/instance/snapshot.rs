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

use std::collections::HashMap;

use carbide_uuid::instance::InstanceId;
use carbide_uuid::instance_type::InstanceTypeId;
use carbide_uuid::machine::{HostMachineId, MachineId};
use carbide_uuid::network_security_group::NetworkSecurityGroupId;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};

use super::config::network::{InstanceNetworkConfig, InstanceNetworkConfigUpdate};
use crate::instance::config::InstanceConfig;
use crate::instance::config::extension_services::InstanceExtensionServicesConfig;
use crate::instance::config::infiniband::InstanceInfinibandConfig;
use crate::instance::config::nvlink::InstanceNvLinkConfig;
use crate::instance::config::spx::InstanceSpxConfig;
use crate::instance::config::tenant_config::TenantConfig;
use crate::instance::status::InstanceStatusObservations;
use crate::metadata::Metadata;
use crate::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
use crate::tenant::TenantOrganizationId;

/// Represents a snapshot view of an `Instance`
///
/// This snapshot is a state-in-time representation of everything that
/// carbide knows about an instance.
/// In order to provide a tenant accurate state of an instance, the state of the
/// host that is hosting the instance also needs to be known.
#[derive(Debug, Clone)]
pub struct InstanceSnapshot {
    /// Instance ID
    pub id: InstanceId,
    /// Machine ID
    pub machine_id: HostMachineId,

    /// InstanceType ID
    pub instance_type_id: Option<InstanceTypeId>,

    /// Instance Metadata
    pub metadata: Metadata,

    /// Instance configuration. This represents the desired status of the Instance
    /// The Instance might not yet be in that state, but work would be underway
    /// to get the Instance into this state
    pub config: InstanceConfig,
    /// Current version of all instance configurations except the networking related ones
    pub config_version: ConfigVersion,

    /// Current version of the networking configuration that is stored as part
    /// of [InstanceConfig::network]
    pub network_config_version: ConfigVersion,

    /// Current version of the infiniband configuration that is stored as part
    /// of [InstanceConfig::infiniband]
    pub ib_config_version: ConfigVersion,

    pub storage_config_version: ConfigVersion,

    /// Current version of the extension services configuration that is stored as part
    /// of [InstanceConfig::extension_services]
    pub extension_services_config_version: ConfigVersion,

    pub nvlink_config_version: ConfigVersion,

    pub spx_config_version: ConfigVersion,

    /// Observed status of the instance
    pub observations: InstanceStatusObservations,

    /// Whether the next boot attempt should run the tenants iPXE script.
    /// This flag is checked by the iPXE handler to determine whether to serve
    /// the tenant's custom script or exit instructions.
    pub use_custom_pxe_on_boot: bool,

    /// Whether a custom PXE reboot has been requested via the API.
    /// This flag is set by the API when a tenant requests a reboot with custom iPXE.
    /// The Ready handler checks this to initiate the HostPlatformConfiguration flow.
    /// The WaitingForRebootToReady handler clears this flag.
    pub custom_pxe_reboot_requested: bool,

    /// The timestamp when deletion for this instance was requested
    pub deleted: Option<chrono::DateTime<chrono::Utc>>,

    /// Update instance network config request.
    pub update_network_config_request: Option<InstanceNetworkConfigUpdate>,
    // There are columns for these but they're unused as of today.
    // pub(crate) requested: chrono::DateTime<chrono::Utc>,
    // pub(crate) started: chrono::DateTime<chrono::Utc>,
    // pub(crate) finished: Option<chrono::DateTime<chrono::Utc>>,
}

/// This represents the structure of an instance we get from postgres via the row_to_json or
/// JSONB_AGG functions. Its fields need to match the column names of the instances table exactly.
/// It's expected that we read this directly from the JSON returned by the query, and then
/// convert it into an InstanceSnapshot.
/// OS-related fields are `pub` so that api-db can merge OS definition with instance overrides when building the snapshot.
#[derive(Serialize, Deserialize)]
pub struct InstanceSnapshotPgJson {
    id: InstanceId,
    machine_id: MachineId,
    name: String,
    description: String,
    labels: HashMap<String, String>,
    network_config: InstanceNetworkConfig,
    network_config_version: String,
    ib_config: InstanceInfinibandConfig,
    ib_config_version: String,
    storage_config_version: String,
    nvlink_config: InstanceNvLinkConfig,
    nvlink_config_version: String,
    spx_config: InstanceSpxConfig,
    spx_config_version: String,
    config_version: String,
    phone_home_last_contact: Option<DateTime<Utc>>,
    use_custom_pxe_on_boot: bool,
    #[serde(default)]
    custom_pxe_reboot_requested: bool,
    tenant_org: Option<String>,
    keyset_ids: Vec<String>,
    hostname: Option<String>,
    pub os_user_data: Option<String>,
    pub os_ipxe_script: String,
    pub os_always_boot_with_ipxe: bool,
    pub os_phone_home_enabled: bool,
    os_image_id: Option<uuid::Uuid>,
    /// Reference to operating_systems table. Instance must have an OS; overrides (above) apply on top of OS.
    pub operating_system_id: Option<uuid::Uuid>,
    instance_type_id: Option<InstanceTypeId>,
    network_security_group_id: Option<NetworkSecurityGroupId>,
    #[serde(default)]
    power_profile: Option<String>,
    extension_services_config: InstanceExtensionServicesConfig,
    extension_services_config_version: String,
    requested: DateTime<Utc>,
    started: DateTime<Utc>,
    finished: Option<DateTime<Utc>>,
    deleted: Option<DateTime<Utc>>,
    update_network_config_request: Option<InstanceNetworkConfigUpdate>,
}

/// Builds an [`InstanceSnapshot`] from DB JSON and a pre-merged [`OperatingSystem`].
/// Use this when the instance row has `operating_system_id` and the OS was loaded and merged with instance overrides.
pub fn from_pg_json_and_os(
    value: InstanceSnapshotPgJson,
    os: OperatingSystem,
) -> Result<InstanceSnapshot, sqlx::Error> {
    let metadata = Metadata {
        name: value.name,
        description: value.description,
        labels: value.labels,
    };

    let tenant_organization_id =
        TenantOrganizationId::try_from(value.tenant_org.unwrap_or_default())
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

    let config = InstanceConfig {
        tenant: TenantConfig {
            tenant_organization_id,
            tenant_keyset_ids: value.keyset_ids,
            hostname: value.hostname,
        },
        os,
        network: value.network_config,
        infiniband: value.ib_config,
        spxconfig: value.spx_config,
        nvlink: value.nvlink_config,
        network_security_group_id: value.network_security_group_id,
        extension_services: value.extension_services_config,
        power_profile: value.power_profile,
    };

    Ok(InstanceSnapshot {
        id: value.id,
        machine_id: value
            .machine_id
            .try_into()
            .map_err(|e| sqlx::Error::ColumnDecode {
                index: "machine_id".to_string(),
                source: Box::new(e),
            })?,
        instance_type_id: value.instance_type_id,
        metadata,
        config,
        config_version: value.config_version.parse().map_err(|e| {
            sqlx::error::Error::ColumnDecode {
                index: "config_version".to_string(),
                source: Box::new(e),
            }
        })?,
        network_config_version: value.network_config_version.parse().map_err(|e| {
            sqlx::error::Error::ColumnDecode {
                index: "network_config_version".to_string(),
                source: Box::new(e),
            }
        })?,
        ib_config_version: value.ib_config_version.parse().map_err(|e| {
            sqlx::error::Error::ColumnDecode {
                index: "ib_config_version".to_string(),
                source: Box::new(e),
            }
        })?,
        nvlink_config_version: value.nvlink_config_version.parse().map_err(|e| {
            sqlx::error::Error::ColumnDecode {
                index: "nvl_config_version".to_string(),
                source: Box::new(e),
            }
        })?,
        spx_config_version: value.spx_config_version.parse().map_err(|e| {
            sqlx::error::Error::ColumnDecode {
                index: "spx_config_version".to_string(),
                source: Box::new(e),
            }
        })?,
        storage_config_version: value.storage_config_version.parse().map_err(|e| {
            sqlx::error::Error::ColumnDecode {
                index: "storage_config_version".to_string(),
                source: Box::new(e),
            }
        })?,
        extension_services_config_version: value
            .extension_services_config_version
            .parse()
            .map_err(|e| sqlx::error::Error::ColumnDecode {
                index: "extension_services_config_version".to_string(),
                source: Box::new(e),
            })?,
        observations: InstanceStatusObservations {
            network: HashMap::default(),
            extension_services: HashMap::default(),
            phone_home_last_contact: value.phone_home_last_contact,
        },
        use_custom_pxe_on_boot: value.use_custom_pxe_on_boot,
        custom_pxe_reboot_requested: value.custom_pxe_reboot_requested,
        deleted: value.deleted,
        update_network_config_request: value.update_network_config_request,
    })
}

impl TryFrom<InstanceSnapshotPgJson> for InstanceSnapshot {
    type Error = sqlx::Error;

    fn try_from(value: InstanceSnapshotPgJson) -> Result<Self, Self::Error> {
        let metadata = Metadata {
            name: value.name,
            description: value.description,
            labels: value.labels,
        };

        let tenant_organization_id =
            TenantOrganizationId::try_from(value.tenant_org.unwrap_or_default())
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        // Derive OS variant from the instance columns.
        // Priority: operating_system_id > os_image_id > inline iPXE script.
        let os = OperatingSystem {
            variant: if let Some(os_id) = value.operating_system_id {
                OperatingSystemVariant::OperatingSystemId(os_id)
            } else if let Some(image_id) = value.os_image_id {
                OperatingSystemVariant::OsImage(image_id)
            } else if !value.os_ipxe_script.trim().is_empty() {
                OperatingSystemVariant::Ipxe(InlineIpxe {
                    ipxe_script: value.os_ipxe_script,
                })
            } else {
                return Err(sqlx::Error::ColumnDecode {
                    index: "os_ipxe_script".to_string(),
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "instance has no operating_system_id, no os_image_id, and no iPXE script",
                    )),
                });
            },
            run_provisioning_instructions_on_every_boot: value.os_always_boot_with_ipxe,
            phone_home_enabled: value.os_phone_home_enabled,
            user_data: value.os_user_data,
        };

        let config = InstanceConfig {
            tenant: TenantConfig {
                tenant_organization_id,
                tenant_keyset_ids: value.keyset_ids,
                hostname: value.hostname,
            },
            os,
            network: value.network_config,
            infiniband: value.ib_config,
            nvlink: value.nvlink_config,
            network_security_group_id: value.network_security_group_id,
            extension_services: value.extension_services_config,
            spxconfig: value.spx_config,
            power_profile: value.power_profile,
        };

        Ok(InstanceSnapshot {
            id: value.id,
            machine_id: value.machine_id.try_into().map_err(|e| {
                sqlx::error::Error::ColumnDecode {
                    index: "machine_id".to_string(),
                    source: Box::new(e),
                }
            })?,
            instance_type_id: value.instance_type_id,
            metadata,
            config,
            config_version: value.config_version.parse().map_err(|e| {
                sqlx::error::Error::ColumnDecode {
                    index: "config_version".to_string(),
                    source: Box::new(e),
                }
            })?,
            network_config_version: value.network_config_version.parse().map_err(|e| {
                sqlx::error::Error::ColumnDecode {
                    index: "network_config_version".to_string(),
                    source: Box::new(e),
                }
            })?,
            ib_config_version: value.ib_config_version.parse().map_err(|e| {
                sqlx::error::Error::ColumnDecode {
                    index: "ib_config_version".to_string(),
                    source: Box::new(e),
                }
            })?,
            nvlink_config_version: value.nvlink_config_version.parse().map_err(|e| {
                sqlx::error::Error::ColumnDecode {
                    index: "nvl_config_version".to_string(),
                    source: Box::new(e),
                }
            })?,
            spx_config_version: value.spx_config_version.parse().map_err(|e| {
                sqlx::error::Error::ColumnDecode {
                    index: "spx_config_version".to_string(),
                    source: Box::new(e),
                }
            })?,
            storage_config_version: value.storage_config_version.parse().map_err(|e| {
                sqlx::error::Error::ColumnDecode {
                    index: "storage_config_version".to_string(),
                    source: Box::new(e),
                }
            })?,
            extension_services_config_version: value
                .extension_services_config_version
                .parse()
                .map_err(|e| sqlx::error::Error::ColumnDecode {
                    index: "extension_services_config_version".to_string(),
                    source: Box::new(e),
                })?,
            observations: InstanceStatusObservations {
                network: HashMap::default(),
                extension_services: HashMap::default(),
                phone_home_last_contact: value.phone_home_last_contact,
            },
            use_custom_pxe_on_boot: value.use_custom_pxe_on_boot,
            custom_pxe_reboot_requested: value.custom_pxe_reboot_requested,
            deleted: value.deleted,
            update_network_config_request: value.update_network_config_request,
            // Unused as of today
            // requested: value.requested,
            // started: value.started,
            // finished: value.finished,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::os::{InlineIpxe, OperatingSystemVariant};

    fn minimal_pg_json() -> InstanceSnapshotPgJson {
        let version = ConfigVersion::initial().version_string();
        InstanceSnapshotPgJson {
            id: InstanceId::from(uuid::Uuid::nil()),
            machine_id: MachineId::from_str(
                "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
            )
            .unwrap(),
            name: String::new(),
            description: String::new(),
            labels: HashMap::new(),
            network_config: InstanceNetworkConfig::default(),
            network_config_version: version.clone(),
            ib_config: InstanceInfinibandConfig::default(),
            ib_config_version: version.clone(),
            storage_config_version: version.clone(),
            nvlink_config: InstanceNvLinkConfig::default(),
            nvlink_config_version: version.clone(),
            spx_config: InstanceSpxConfig::default(),
            spx_config_version: version.clone(),
            config_version: version.clone(),
            phone_home_last_contact: None,
            use_custom_pxe_on_boot: false,
            custom_pxe_reboot_requested: false,
            tenant_org: Some("TenantA".to_string()),
            keyset_ids: vec![],
            hostname: None,
            os_user_data: None,
            os_ipxe_script: String::new(),
            os_always_boot_with_ipxe: false,
            os_phone_home_enabled: false,
            os_image_id: None,
            operating_system_id: None,
            instance_type_id: None,
            network_security_group_id: None,
            power_profile: None,
            extension_services_config: InstanceExtensionServicesConfig::default(),
            extension_services_config_version: version,
            requested: Utc::now(),
            started: Utc::now(),
            finished: None,
            deleted: None,
            update_network_config_request: None,
        }
    }

    #[test]
    fn test_from_pg_json_and_os_uses_provided_os() {
        let mut pg_json = minimal_pg_json();
        pg_json.operating_system_id = Some(Uuid::nil());
        pg_json.power_profile = Some("balanced".to_string());
        let os = OperatingSystem {
            user_data: Some("user-data".to_string()),
            variant: OperatingSystemVariant::Ipxe(InlineIpxe {
                ipxe_script: "script-from-os".to_string(),
            }),
            run_provisioning_instructions_on_every_boot: true,
            phone_home_enabled: true,
        };
        let snapshot = from_pg_json_and_os(pg_json, os.clone()).unwrap();
        assert_eq!(snapshot.config.os.variant, os.variant);
        assert_eq!(snapshot.config.os.user_data, os.user_data);
        assert_eq!(snapshot.config.os.phone_home_enabled, os.phone_home_enabled);
        assert_eq!(snapshot.config.power_profile.as_deref(), Some("balanced"));
        if let OperatingSystemVariant::Ipxe(ipxe) = &snapshot.config.os.variant {
            assert_eq!(ipxe.ipxe_script, "script-from-os");
        } else {
            panic!("expected Ipxe variant");
        }
    }

    /// `InstanceSnapshot::try_from` derives the OS variant from the legacy
    /// instance columns (priority: operating_system_id > os_image_id > inline
    /// iPXE). Each row mutates a minimal pg-json row, then projects the converted
    /// snapshot to the fields under test: (os variant, user_data, phone_home,
    /// power_profile).
    #[test]
    fn test_try_from_derives_os_from_instance_columns() {
        let image_uuid = uuid::uuid!("a1b2c3d4-e5f6-4780-a123-456789abcdef");
        let os_uuid = uuid::uuid!("b2c3d4e5-f6a7-4890-b234-567890abcdef");

        scenarios!(
            // Apply the row's mutation to a minimal pg-json, convert, and project
            // to the asserted OS fields. The error type (sqlx::Error) is not
            // PartialEq, so on failure we discard it.
            run = |mutate| {
                let mut pg_json = minimal_pg_json();
                mutate(&mut pg_json);
                InstanceSnapshot::try_from(pg_json)
                    .map(|snapshot| {
                        (
                            snapshot.config.os.variant,
                            snapshot.config.os.user_data,
                            snapshot.config.os.phone_home_enabled,
                            snapshot.config.power_profile,
                        )
                    })
                    .map_err(drop)
            };
            "legacy inline iPXE derives Ipxe variant with user_data and phone_home" {
                Box::new(|pg: &mut InstanceSnapshotPgJson| {
                    pg.operating_system_id = None;
                    pg.os_image_id = None;
                    pg.os_ipxe_script = "legacy-inline-script".to_string();
                    pg.os_user_data = Some("legacy-user-data".to_string());
                    pg.os_phone_home_enabled = true;
                    pg.power_profile = Some("balanced".to_string());
                }) as Box<dyn Fn(&mut InstanceSnapshotPgJson)> => Yields((
                    OperatingSystemVariant::Ipxe(InlineIpxe {
                        ipxe_script: "legacy-inline-script".to_string(),
                    }),
                    Some("legacy-user-data".to_string()),
                    true,
                    Some("balanced".to_string()),
                )),
            }

            "legacy os_image_id derives OsImage variant (iPXE script ignored)" {
                Box::new(move |pg: &mut InstanceSnapshotPgJson| {
                    pg.operating_system_id = None;
                    pg.os_image_id = Some(image_uuid);
                    pg.os_ipxe_script = "ignored".to_string();
                }) => Yields((OperatingSystemVariant::OsImage(image_uuid), None, false, None)),
            }

            "operating_system_id takes priority over image and iPXE" {
                Box::new(move |pg: &mut InstanceSnapshotPgJson| {
                    pg.operating_system_id = Some(os_uuid);
                    pg.os_image_id = None;
                    pg.os_ipxe_script = String::new();
                }) => Yields((
                    OperatingSystemVariant::OperatingSystemId(os_uuid),
                    None,
                    false,
                    None,
                )),
            }
        );
    }

    #[test]
    fn test_try_from_errors_when_no_os_and_no_ipxe_script() {
        let mut pg_json = minimal_pg_json();
        pg_json.operating_system_id = None;
        pg_json.os_image_id = None;
        pg_json.os_ipxe_script = String::new();
        let err = InstanceSnapshot::try_from(pg_json).unwrap_err();
        assert!(matches!(err, sqlx::Error::ColumnDecode { .. }));
        assert!(format!("{err}").contains("no operating_system_id"));
        assert!(format!("{err}").contains("no iPXE script"));
    }
}
