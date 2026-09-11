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
use std::collections::BTreeMap;
use std::fmt::Write;

use carbide_uuid::machine::{MachineId, MachineIdSubtypeTrait};
use chrono::Utc;
use futures_util::stream::StreamExt;
use itertools::Itertools;
use model::hardware_info::HardwareInfo;
use model::machine::Machine;
use model::machine::capabilities::{MachineCapabilitiesSet, MachineCapabilityInfiniband};
use model::machine::machine_search_config::MachineSearchConfig;
use model::sku::{
    SKU_VERSION_WITH_DRIVE_LOCATION, Sku, SkuComponentChassis, SkuComponentCpu, SkuComponentGpu,
    SkuComponentInfinibandDevices, SkuComponentMemory, SkuComponentStorage, SkuComponentTpm,
    SkuComponents, diff_skus,
};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, ObjectFilter, Transaction, machine};

/// The current version of the SKU format.  The state machine will create older
/// versions from hardware using the currently assigned sku's version so that
/// SKUs can maintain backward compatibility
pub const CURRENT_SKU_VERSION: u32 = 5;

/// Find a SKU that matches the specified SKU using the same comparison that
/// the SKU validation code uses. (i.e. the description, id and others are not compared)
///
/// The specified SKU must not exist in the DB (otherwise it will always be the match).
pub async fn find_matching(
    txn: &mut PgConnection,
    sku: &Sku,
) -> Result<Option<Sku>, DatabaseError> {
    find_matching_with_exclusion(txn, sku, None).await
}

pub async fn find_matching_with_exclusion(
    txn: &mut PgConnection,
    sku: &Sku,
    excluded_sku_id: Option<&String>,
) -> Result<Option<Sku>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT * FROM machine_skus");
    if let Some(excluded_sku_id) = excluded_sku_id {
        builder.push(" WHERE id != ");
        builder.push_bind(excluded_sku_id);
    }

    let sql = builder.sql().as_str().to_string();
    let mut sku_stream = builder.build_query_as().fetch(txn);

    while let Some(result) = sku_stream.next().await {
        match result {
            Ok(existing_sku) => {
                let diffs = diff_skus(sku, &existing_sku);
                if diffs.is_empty() {
                    return Ok(Some(existing_sku));
                }
            }
            Err(sqlx::Error::RowNotFound) => {}
            Err(e) => return Err(DatabaseError::query(&sql, e)),
        }
    }

    Ok(None)
}

/// Validate storage components of an expected SKU being persisted.
///
/// Rejects uncompilable PCI patterns (caught at authoring time rather than at
/// validation time), and rejects v5 storage entries that carry no constraints
/// at all (no size bounds, no PCI patterns). Such entries would silently accept
/// any drive at any location, providing weaker guarantees than v4 model
/// matching. This most commonly occurs when a v5 SKU is auto-generated from
/// hardware_info that predates the size_mb/pci_path fields; those SKUs should
/// not be persisted until the hardware has been re-enumerated.
fn validate_storage_for_create(sku: &Sku) -> Result<(), DatabaseError> {
    for storage in &sku.components.storage {
        for pattern in &storage.pci_patterns {
            regex::Regex::new(pattern).map_err(|err| {
                DatabaseError::InvalidArgument(format!(
                    "invalid storage PCI pattern \"{pattern}\": {err}"
                ))
            })?;
        }
        if let (Some(min), Some(max)) = (storage.min_size_mb, storage.max_size_mb)
            && min > max
        {
            return Err(DatabaseError::InvalidArgument(format!(
                "storage entry (model {:?}) has min_size_mb ({min}) greater than max_size_mb ({max})",
                storage.model
            )));
        }
        if sku.schema_version >= SKU_VERSION_WITH_DRIVE_LOCATION
            && storage.pci_patterns.is_empty()
            && storage.min_size_mb.is_none()
            && storage.max_size_mb.is_none()
        {
            return Err(DatabaseError::InvalidArgument(format!(
                "v5 storage entry (model {:?}) has no size bounds or PCI patterns; \
                 re-enumerate the machine's hardware before creating this SKU",
                storage.model
            )));
        }
    }
    Ok(())
}

pub async fn create(txn: &mut PgConnection, sku: &Sku) -> Result<(), DatabaseError> {
    if sku.schema_version != CURRENT_SKU_VERSION {
        return Err(DatabaseError::InvalidArgument(
            "SKU version is no longer supported".to_string(),
        ));
    }

    if sku.id.is_empty() {
        return Err(DatabaseError::InvalidArgument(
            "SKU ID must not be empty".to_string(),
        ));
    }

    validate_storage_for_create(sku)?;

    let mut inner_txn = Transaction::begin_inner(txn).await?;

    let query = "LOCK TABLE machine_skus IN ACCESS EXCLUSIVE MODE";
    sqlx::query(query)
        .execute(inner_txn.as_pgconn())
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if let Some(existing_sku) = find_matching(&mut inner_txn, sku).await? {
        return Err(DatabaseError::InvalidArgument(format!(
            "Specified SKU matches SKU with ID: {}",
            existing_sku.id
        )));
    }

    // purposely leaves out Created.  it will be generated by the DB.
    let query = "INSERT INTO machine_skus (id, description, components, schema_version, device_type) values ($1, $2, $3, $4, $5) RETURNING id";

    sqlx::query_as::<_, ()>(query)
        .bind(&sku.id)
        .bind(&sku.description)
        .bind(sqlx::types::Json(&sku.components))
        .bind(sku.schema_version as i32)
        .bind(&sku.device_type)
        .fetch_one(inner_txn.as_pgconn())
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|e| e.is_unique_violation())
            {
                DatabaseError::AlreadyFoundError {
                    kind: "SKU",
                    id: sku.id.clone(),
                }
            } else {
                DatabaseError::new("create sku", e)
            }
        })?;

    inner_txn.commit().await?;

    Ok(())
}

pub async fn delete(txn: &mut PgConnection, sku_id: &str) -> Result<String, DatabaseError> {
    // purposely leaves out Created.  it will be generated by the DB.
    let query = "DELETE FROM machine_skus WHERE id=$1 RETURNING id";

    let (id,): (String,) = sqlx::query_as(query)
        .bind(sku_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(id)
}

pub async fn get_sku_ids(txn: impl DbReader<'_>) -> Result<Vec<String>, DatabaseError> {
    let query = "SELECT id FROM machine_skus";

    let skus: Vec<(String,)> = sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("get sku ids", e))?;

    Ok(skus.into_iter().map(|v| v.0).collect())
}

pub async fn find(
    txn: &mut PgConnection,
    sku_ids: &[impl AsRef<str>],
) -> Result<Vec<Sku>, DatabaseError> {
    if sku_ids.is_empty() {
        return Ok(Vec::new());
    }

    let query = "SELECT * FROM machine_skus WHERE id=ANY($1)";

    let skus: Vec<Sku> = sqlx::query_as(query)
        .bind(sku_ids.iter().map(AsRef::as_ref).collect::<Vec<_>>())
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("find skus", e))?;

    Ok(skus)
}

pub async fn update_metadata(
    txn: &mut PgConnection,
    sku_id: String,
    description: Option<String>,
    device_type: Option<String>,
) -> Result<(), DatabaseError> {
    if description.is_none() && device_type.is_none() {
        return Err(DatabaseError::new(
            "Update SKU Metadata",
            sqlx::Error::InvalidArgument("desciption and/or device_type required".to_string()),
        ));
    }

    let mut builder = sqlx::QueryBuilder::new("UPDATE machine_skus SET ".to_string());
    if let Some(description) = description {
        builder.push(" description = ");
        builder.push_bind(description);
        if device_type.is_some() {
            builder.push(",");
        }
    }

    if let Some(device_type) = device_type {
        builder.push(" device_type = ");
        builder.push_bind(device_type);
    }

    builder.push(" WHERE id = ");
    builder.push_bind(sku_id);
    builder.push(" RETURNING id");

    let _: (String,) = builder
        .build_query_as()
        .fetch_one(&mut *txn)
        .await
        .map_err(|err| DatabaseError::query(builder.sql(), err))?;

    Ok(())
}

pub async fn replace(txn: &mut PgConnection, sku: &Sku) -> Result<Sku, DatabaseError> {
    if sku.schema_version != CURRENT_SKU_VERSION {
        return Err(DatabaseError::InvalidArgument(
            "SKU version is no longer supported".to_string(),
        ));
    }

    if sku.id.is_empty() {
        return Err(DatabaseError::InvalidArgument(
            "SKU ID must not be empty".to_string(),
        ));
    }

    validate_storage_for_create(sku)?;

    let mut inner_txn = Transaction::begin_inner(txn).await?;

    let query = "LOCK TABLE machine_skus IN ACCESS EXCLUSIVE MODE";
    sqlx::query(query)
        .execute(inner_txn.as_pgconn())
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if let Some(existing_sku) =
        find_matching_with_exclusion(&mut inner_txn, sku, Some(&sku.id)).await?
    {
        return Err(DatabaseError::InvalidArgument(format!(
            "Specified SKU matches SKU with ID: {}",
            existing_sku.id
        )));
    }

    let query = "UPDATE machine_skus set description=$1, components=$2, schema_version=$3, device_type=$4 WHERE id=$5 RETURNING id";

    sqlx::query_as::<_, ()>(query)
        .bind(&sku.description)
        .bind(sqlx::types::Json(&sku.components))
        .bind(sku.schema_version as i32)
        .bind(&sku.device_type)
        .bind(&sku.id)
        .fetch_one(inner_txn.as_pgconn())
        .await
        .map_err(|e| DatabaseError::new("replace sku: update", e))?;

    crate::machine::update_sku_status_verify_request_time_for_sku(&mut inner_txn, &sku.id).await?;

    inner_txn.commit().await?;

    find(txn, std::slice::from_ref(&sku.id))
        .await?
        .pop()
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "SKU",
            id: sku.id.clone(),
        })
}

pub async fn generate_sku_from_machine(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Sku, DatabaseError> {
    generate_sku_from_machine_at_version(txn, machine_id, CURRENT_SKU_VERSION).await
}

pub async fn generate_sku_from_machine_at_version(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
    schema_version: u32,
) -> Result<Sku, DatabaseError> {
    match schema_version {
        0 | 1 => generate_sku_from_machine_at_version_0_or_1(txn, machine_id, schema_version).await,
        2 => generate_sku_from_machine_at_version_2(txn, machine_id).await,
        3 => generate_sku_from_machine_at_version_3(txn, machine_id).await,
        4 => generate_sku_from_machine_at_version_4(txn, machine_id).await,
        5 => generate_sku_from_machine_at_version_5(txn, machine_id).await,
        _ => Err(DatabaseError::new(
            "generate_sku_from_machine_at_version",
            sqlx::Error::RowNotFound,
        )),
    }
}

fn memory_components_from_hardware_info(
    hardware_info: &HardwareInfo,
) -> (BTreeMap<(String, u32), SkuComponentMemory>, u64) {
    let mut mem_components: BTreeMap<(String, u32), SkuComponentMemory> = BTreeMap::default();
    let mut total_mem = 0u64;
    for mem in &hardware_info.memory_devices {
        if let Some(cap) = mem.size_mb {
            total_mem = total_mem.saturating_add((cap as u64).saturating_mul(mem.count as u64));
            let key = (mem.mem_type.clone().unwrap_or_default(), cap);
            mem_components
                .entry(key.clone())
                .and_modify(|entry| entry.count = entry.count.saturating_add(mem.count))
                .or_insert(SkuComponentMemory {
                    capacity_mb: key.1,
                    memory_type: key.0,
                    count: mem.count,
                });
        }
    }
    (mem_components, total_mem)
}

pub async fn generate_sku_from_machine_at_version_0_or_1(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
    schema_version: u32,
) -> Result<Sku, DatabaseError> {
    let created = Utc::now();

    let Some(machine) = crate::machine::find(
        txn,
        crate::ObjectFilter::One(*machine_id),
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?
    .into_iter()
    .next() else {
        return Err(DatabaseError::new(
            "generate sku: find machine",
            sqlx::Error::RowNotFound,
        ));
    };

    let Some(hardware_info) = machine.status.hardware_info.as_ref() else {
        return Err(DatabaseError::new(
            "generate sku: load hardware info",
            sqlx::Error::RowNotFound,
        ));
    };

    let chassis = SkuComponentChassis {
        vendor: hardware_info
            .dmi_data
            .as_ref()
            .map(|dd| dd.sys_vendor.clone())
            .unwrap_or_default(),
        model: hardware_info
            .dmi_data
            .as_ref()
            .map(|dd| dd.product_name.clone())
            .unwrap_or_default(),
        architecture: hardware_info.machine_type.to_string(),
    };

    let mut gpu_components: BTreeMap<(String, String), SkuComponentGpu> = BTreeMap::default();
    for gpu in &hardware_info.gpus {
        let vendor = "NVIDIA".to_string();
        let key = (gpu.name.clone(), gpu.total_memory.clone());
        gpu_components
            .entry(key)
            .and_modify(|entry| entry.count = entry.count.saturating_add(1))
            .or_insert(SkuComponentGpu {
                vendor,
                model: gpu.name.clone(),
                count: 1,
                total_memory: gpu.total_memory.clone(),
            });
    }

    let (mem_components, total_mem) = memory_components_from_hardware_info(hardware_info);

    let ib_capabilities = MachineCapabilityInfiniband::from_ib_interfaces_and_status(
        &hardware_info.infiniband_interfaces,
        machine.status.infiniband_status_observation.as_ref(),
    );
    let ib_components: Vec<SkuComponentInfinibandDevices> = ib_capabilities
        .into_iter()
        .map(|cap| SkuComponentInfinibandDevices {
            vendor: cap.vendor,
            model: cap.name,
            count: cap.count,
            inactive_devices: cap.inactive_devices,
        })
        .collect();

    let mut description = format!(
        "{}; {}xCPU; {}xGPU; {}",
        chassis.model,
        hardware_info
            .cpu_info
            .iter()
            .map(|v| v.sockets)
            .sum::<u32>(),
        gpu_components.values().map(|v| v.count).sum::<u32>(),
        ::carbide_utils::sku::capacity_string(total_mem)
    );
    let num_ib_devices = ib_components.iter().map(|c| c.count).sum::<u32>();
    if num_ib_devices != 0 {
        write!(&mut description, "; {num_ib_devices}xIB").unwrap();
    }

    let storage = if schema_version >= 1 {
        let mut storage: BTreeMap<String, SkuComponentStorage> = BTreeMap::new();
        for block_device in &hardware_info.block_devices {
            storage
                .entry(block_device.model.clone())
                .and_modify(|s| s.count += 1)
                .or_insert(SkuComponentStorage {
                    model: block_device.model.clone(),
                    count: 1,
                    min_size_mb: None,
                    max_size_mb: None,
                    pci_patterns: Vec::new(),
                });
        }
        storage
    } else {
        BTreeMap::default()
    };

    Ok(Sku {
        schema_version,
        id: format!("{} {}", chassis.model, Utc::now()),
        description,
        created,
        components: SkuComponents {
            chassis,
            cpus: hardware_info
                .cpu_info
                .iter()
                .map(SkuComponentCpu::from)
                .collect(),
            gpus: gpu_components.into_values().collect(),
            memory: mem_components.into_values().collect(),
            infiniband_devices: ib_components,
            storage: storage.into_values().collect(),
            tpm: None,
        },
        device_type: None,
    })
}

pub fn generate_base_sku_from_hardware(
    machine: &Machine<impl MachineIdSubtypeTrait>,
    schema_version: u32,
    hardware_info: &HardwareInfo,
) -> Sku {
    let created = Utc::now();

    let capabilities = MachineCapabilitiesSet::from_hardware_info(
        hardware_info,
        machine.status.infiniband_status_observation.as_ref(),
        machine.associated_dpu_machine_ids(),
        &machine.status.interfaces,
    );

    let chassis = SkuComponentChassis {
        vendor: hardware_info
            .dmi_data
            .as_ref()
            .map(|dd| dd.sys_vendor.clone())
            .unwrap_or_default(),
        model: hardware_info
            .dmi_data
            .as_ref()
            .map(|dd| dd.product_name.clone())
            .unwrap_or_default(),
        architecture: hardware_info.machine_type.to_string(),
    };

    let cpus: Vec<SkuComponentCpu> = capabilities
        .cpu
        .into_iter()
        .map(|c| SkuComponentCpu {
            vendor: c.vendor.unwrap_or_default(),
            model: c.name,
            thread_count: c.threads.unwrap_or_default(),
            count: c.count,
        })
        .sorted()
        .collect();

    let gpus: Vec<SkuComponentGpu> = capabilities
        .gpu
        .into_iter()
        .map(|g| SkuComponentGpu {
            vendor: g.vendor.unwrap_or("NVIDIA".to_owned()),
            model: g.name,
            total_memory: g.memory_capacity.unwrap_or_default(),
            count: g.count,
        })
        .sorted()
        .collect();

    let (mem_components, total_mem) = memory_components_from_hardware_info(hardware_info);

    let infiniband_devices: Vec<SkuComponentInfinibandDevices> = capabilities
        .infiniband
        .into_iter()
        .map(|cap| SkuComponentInfinibandDevices {
            vendor: cap.vendor,
            model: cap.name,
            count: cap.count,
            inactive_devices: cap.inactive_devices,
        })
        .sorted()
        .collect();

    let mut description = format!(
        "{}; {}xCPU; {}xGPU; {}",
        chassis.model,
        cpus.iter().map(|v| v.count).sum::<u32>(),
        gpus.iter().map(|v| v.count).sum::<u32>(),
        ::carbide_utils::sku::capacity_string(total_mem)
    );
    let num_ib_devices = infiniband_devices.iter().map(|c| c.count).sum::<u32>();
    if num_ib_devices != 0 {
        write!(&mut description, "; {num_ib_devices}xIB").unwrap();
    }

    Sku {
        schema_version,
        id: format!("{} {}", chassis.model, Utc::now()),
        description,
        created,
        components: SkuComponents {
            chassis,
            cpus,
            gpus,
            memory: mem_components.into_values().collect(),
            infiniband_devices,
            storage: Vec::default(),
            tpm: None,
        },
        device_type: None,
    }
}

pub async fn generate_sku_from_machine_at_version_2(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Sku, DatabaseError> {
    let Some(machine) = crate::machine::find(
        txn,
        crate::ObjectFilter::One(*machine_id),
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?
    .into_iter()
    .next() else {
        return Err(DatabaseError::new(
            "generate sku: find machine (v2)",
            sqlx::Error::RowNotFound,
        ));
    };

    let Some(hardware_info) = machine.status.hardware_info.as_ref() else {
        return Err(DatabaseError::new(
            "generate sku: load hardware info (v2)",
            sqlx::Error::RowNotFound,
        ));
    };

    let mut sku = generate_base_sku_from_hardware(&machine, 2, hardware_info);

    // Storage cannot be pulled from capabilities (yet).  The block device hardware inventory has duplicate entries
    // for disk and partitions on that disk.  The NVME device inventory is a duplicate of the disk entries
    // in the block device, and so is ignored.  TODO: move to use capabilities when available.
    let mut storage: BTreeMap<String, SkuComponentStorage> = BTreeMap::default();
    for s in hardware_info
        .block_devices
        .iter()
        .filter(|s| s.device_type == "disk")
    {
        storage
            .entry(s.model.clone())
            .and_modify(|s| s.count += 1)
            .or_insert(SkuComponentStorage {
                model: s.model.clone(),
                count: 1,
                min_size_mb: None,
                max_size_mb: None,
                pci_patterns: Vec::new(),
            });
    }

    sku.components.storage = storage.into_values().collect();

    Ok(sku)
}

pub async fn generate_sku_from_machine_at_version_3(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Sku, DatabaseError> {
    let Some(machine) = crate::machine::find(
        txn,
        crate::ObjectFilter::One(*machine_id),
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?
    .into_iter()
    .next() else {
        return Err(DatabaseError::new(
            "generate sku: find machine (v3)",
            sqlx::Error::RowNotFound,
        ));
    };

    let Some(hardware_info) = machine.status.hardware_info.as_ref() else {
        return Err(DatabaseError::new(
            "generate sku: load hardware info (v3)",
            sqlx::Error::RowNotFound,
        ));
    };

    let mut sku = generate_base_sku_from_hardware(&machine, 3, hardware_info);

    // Storage cannot be pulled from capabilities (yet).  The block devices are no longer used
    // as RAID devices cause issues by created additional devices (and how depends on which
    // RAID card is used) Note that this will include RAID devices themselves, but not the
    // intermediate devices created by the RAID device.
    let mut storage: BTreeMap<String, SkuComponentStorage> = BTreeMap::default();
    hardware_info.nvme_devices.iter().for_each(|nvme| {
        storage
            .entry(nvme.model.clone())
            .and_modify(|s| s.count += 1)
            .or_insert(SkuComponentStorage {
                model: nvme.model.clone(),
                count: 1,
                min_size_mb: None,
                max_size_mb: None,
                pci_patterns: Vec::new(),
            });
    });
    sku.components.storage = storage.into_values().collect();

    Ok(sku)
}

pub async fn generate_sku_from_machine_at_version_4(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Sku, DatabaseError> {
    let Some(machine) = machine::find(
        txn,
        ObjectFilter::One(*machine_id),
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?
    .into_iter()
    .next() else {
        return Err(DatabaseError::new(
            "generate sku: find machine (v4)",
            sqlx::Error::RowNotFound,
        ));
    };

    let Some(hardware_info) = machine.status.hardware_info.as_ref() else {
        return Err(DatabaseError::new(
            "generate sku: load hardware info (v4)",
            sqlx::Error::RowNotFound,
        ));
    };

    let mut sku = generate_base_sku_from_hardware(&machine, 4, hardware_info);

    // Storage cannot be pulled from capabilities (yet).  The block devices are no longer used
    // as RAID devices cause issues by created additional devices (and how depends on which
    // RAID card is used) Note that this will include RAID devices themselves, but not the
    // intermediate devices created by the RAID device.
    let mut storage: BTreeMap<String, SkuComponentStorage> = BTreeMap::default();
    hardware_info.nvme_devices.iter().for_each(|nvme| {
        storage
            .entry(nvme.model.clone())
            .and_modify(|s| s.count += 1)
            .or_insert(SkuComponentStorage {
                model: nvme.model.clone(),
                count: 1,
                min_size_mb: None,
                max_size_mb: None,
                pci_patterns: Vec::new(),
            });
    });
    sku.components.storage = storage.into_values().collect();

    // Vendor and Model fields do not contain useful information.  They seem limited and encoded somehow.
    // We really only care about the spec version supported and that a TPM exists.
    sku.components.tpm = hardware_info
        .tpm_description
        .as_ref()
        .map(|tpm| SkuComponentTpm {
            vendor: tpm.vendor.clone(),
            version: tpm.tpm_spec.clone(),
        });

    Ok(sku)
}

pub async fn generate_sku_from_machine_at_version_5(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Sku, DatabaseError> {
    let Some(machine) = machine::find(
        txn,
        ObjectFilter::One(*machine_id),
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?
    .into_iter()
    .next() else {
        return Err(DatabaseError::new(
            "generate sku: find machine (v5)",
            sqlx::Error::RowNotFound,
        ));
    };

    let Some(hardware_info) = machine.status.hardware_info.as_ref() else {
        return Err(DatabaseError::new(
            "generate sku: load hardware info (v5)",
            sqlx::Error::RowNotFound,
        ));
    };

    let mut sku = generate_base_sku_from_hardware(&machine, 5, hardware_info);

    // Unlike earlier versions, v5 records one storage entry per NVMe drive so
    // each drive's size and PCI location can be validated individually. The
    // discovered size is stored as an exact point (min == max) and the concrete
    // sysfs/PCI path is stored as the drive's single "pattern". An expected SKU
    // authored from this can then widen the size range or replace the literal
    // path with a regex. Drives are ordered by path for deterministic output.
    //
    // size_mb and pci_path may be absent on hardware_info records that predate
    // the v5 fields (discovered before PR #3717). Rather than failing generation
    // and wedging the machine, we include the drive with whatever fields are
    // present. A drive without a path will not match any location-constrained
    // expected group (which is correct — the mismatch is reported as a diff),
    // and a drive without a size satisfies only unconstrained size ranges. The
    // machine self-heals once hardware re-enumeration populates the new fields.
    let mut storage: Vec<SkuComponentStorage> = hardware_info
        .nvme_devices
        .iter()
        .map(|nvme| SkuComponentStorage {
            model: nvme.model.clone(),
            count: 1,
            min_size_mb: nvme.size_mb,
            max_size_mb: nvme.size_mb,
            pci_patterns: nvme
                .pci_path
                .as_ref()
                .map(|p| vec![p.clone()])
                .unwrap_or_default(),
        })
        .collect();
    storage.sort();
    sku.components.storage = storage;

    // Vendor and Model fields do not contain useful information.  They seem limited and encoded somehow.
    // We really only care about the spec version supported and that a TPM exists.
    sku.components.tpm = hardware_info
        .tpm_description
        .as_ref()
        .map(|tpm| SkuComponentTpm {
            vendor: tpm.vendor.clone(),
            version: tpm.tpm_spec.clone(),
        });

    Ok(sku)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use model::hardware_info::MemoryDeviceGroup;
    use model::test_support::machine_snapshot::host_machine;

    use super::*;

    fn group(mem_type: Option<&str>, size_mb: Option<u32>, count: u32) -> MemoryDeviceGroup {
        MemoryDeviceGroup {
            size_mb,
            mem_type: mem_type.map(str::to_owned),
            count,
        }
    }

    fn mem(memory_type: &str, capacity_mb: u32, count: u32) -> SkuComponentMemory {
        SkuComponentMemory {
            memory_type: memory_type.to_owned(),
            capacity_mb,
            count,
        }
    }

    /// Runs `generate_base_sku_from_hardware` over `devices` and returns the resulting
    /// memory components, sorted for order-independent comparison.
    fn generated_memory(devices: Vec<MemoryDeviceGroup>) -> Vec<SkuComponentMemory> {
        let machine = host_machine();
        let hardware_info = HardwareInfo {
            memory_devices: devices,
            ..Default::default()
        };
        let sku = generate_base_sku_from_hardware(&machine, CURRENT_SKU_VERSION, &hardware_info);
        let mut memory = sku.components.memory;
        memory.sort();
        memory
    }

    #[test]
    fn generate_base_sku_from_hardware_groups_memory_devices() {
        check_values(
            [
                Check {
                    scenario: "no memory devices produce no memory components",
                    input: vec![],
                    expect: vec![],
                },
                Check {
                    scenario: "a single group becomes a single component",
                    input: vec![group(Some("DDR5"), Some(65536), 8)],
                    expect: vec![mem("DDR5", 65536, 8)],
                },
                Check {
                    scenario: "groups with distinct type or size stay separate",
                    input: vec![
                        group(Some("DDR5"), Some(65536), 8),
                        group(Some("DDR5"), Some(32768), 4),
                        group(Some("DDR4"), Some(65536), 2),
                    ],
                    expect: {
                        let mut expect = vec![
                            mem("DDR5", 65536, 8),
                            mem("DDR5", 32768, 4),
                            mem("DDR4", 65536, 2),
                        ];
                        expect.sort();
                        expect
                    },
                },
                Check {
                    scenario: "non-consecutive groups with the same type and size merge",
                    input: vec![
                        group(Some("DDR5"), Some(65536), 4),
                        group(Some("DDR4"), Some(32768), 1),
                        group(Some("DDR5"), Some(65536), 4),
                    ],
                    expect: {
                        let mut expect = vec![mem("DDR5", 65536, 8), mem("DDR4", 32768, 1)];
                        expect.sort();
                        expect
                    },
                },
                Check {
                    scenario: "groups without a size are dropped from the SKU",
                    input: vec![
                        group(Some("DDR5"), None, 4),
                        group(Some("DDR5"), Some(65536), 1),
                    ],
                    expect: vec![mem("DDR5", 65536, 1)],
                },
                Check {
                    scenario: "a missing memory type defaults to an empty string",
                    input: vec![group(None, Some(65536), 2)],
                    expect: vec![mem("", 65536, 2)],
                },
                Check {
                    scenario: "merged counts saturate instead of overflowing",
                    input: vec![
                        group(Some("DDR5"), Some(u32::MAX), u32::MAX),
                        group(Some("DDR5"), Some(u32::MAX), u32::MAX),
                    ],
                    expect: vec![mem("DDR5", u32::MAX, u32::MAX)],
                },
            ],
            generated_memory,
        );
    }
}
