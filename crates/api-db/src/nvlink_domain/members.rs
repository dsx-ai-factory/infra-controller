/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Shared position lookup for Domain ingestion and maintenance.

use std::collections::HashMap;

use carbide_uuid::machine::{HostMachineId, MachineId};
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use config_version::{ConfigVersion, Versioned};
use model::machine::ManagedHostState;
use model::nvlink_domain::NvLinkDomainMember;
use model::rack_type::RackCapabilityType;
use model::switch::SwitchControllerState;
use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;

use crate::{DatabaseError, DatabaseResult};

/// An actual discovered tray and its observed, versioned lifecycle state.
#[derive(Clone, Debug)]
pub enum NvLinkDomainTray {
    /// A stable compute host; predicted hosts and DPUs are excluded.
    Compute {
        /// Actual host identity.
        machine_id: HostMachineId,
        /// State observed during inventory lookup; no readiness filtering is applied.
        controller_state: Box<Versioned<ManagedHostState>>,
    },
    /// A switch that has not been soft-deleted.
    Switch {
        /// Actual switch identity.
        switch_id: SwitchId,
        /// State observed during inventory lookup; no readiness filtering is applied.
        controller_state: Box<Versioned<SwitchControllerState>>,
    },
}

/// One declared position mapped to its actual rack and optional discovered device.
#[derive(Clone, Debug)]
pub struct ResolvedNvLinkDomainMember {
    /// Original position, preserving the Domain declaration's order.
    pub member: NvLinkDomainMember,
    /// RackGroup's rack identity at the declared rack index.
    pub rack_id: RackId,
    /// None when no actual tray has been discovered at this position.
    pub tray: Option<NvLinkDomainTray>,
}

type Position = (RackId, i64);

struct TrayPosition {
    rack_id: RackId,
    component_type: RackCapabilityType,
    slot_index: u32,
}

#[derive(Default)]
struct Inventory {
    compute: HashMap<Position, NvLinkDomainTray>,
    switches: HashMap<Position, NvLinkDomainTray>,
}

impl Inventory {
    fn get(&self, position: &TrayPosition) -> Option<NvLinkDomainTray> {
        let key = (position.rack_id.clone(), i64::from(position.slot_index));
        match position.component_type {
            RackCapabilityType::Compute => self.compute.get(&key),
            RackCapabilityType::Switch => self.switches.get(&key),
            RackCapabilityType::PowerShelf => None,
        }
        .cloned()
    }
}

fn positions(
    members: &[TrayPosition],
    component_type: RackCapabilityType,
) -> (Vec<String>, Vec<i64>) {
    members
        .iter()
        .filter(|member| member.component_type == component_type)
        .map(|member| (member.rack_id.to_string(), i64::from(member.slot_index)))
        .unzip()
}

fn insert(
    inventory: &mut HashMap<Position, NvLinkDomainTray>,
    rack_id: RackId,
    slot_number: i32,
    tray: NvLinkDomainTray,
) -> DatabaseResult<()> {
    if inventory
        .insert((rack_id.clone(), i64::from(slot_number)), tray)
        .is_some()
    {
        return Err(DatabaseError::FailedPrecondition(format!(
            "multiple actual trays of the same type occupy rack {rack_id} slot {slot_number}"
        )));
    }
    Ok(())
}

async fn load_inventory(
    txn: &mut PgConnection,
    members: &[TrayPosition],
) -> DatabaseResult<Inventory> {
    let mut inventory = Inventory::default();
    let (rack_ids, slots) = positions(members, RackCapabilityType::Compute);
    if !rack_ids.is_empty() {
        // SlotID is persisted as slot_number; tray_index is a separate RMS location field.
        let query = "SELECT id, rack_id, slot_number, controller_state, controller_state_version
            FROM machines WHERE (rack_id, slot_number) IN
            (SELECT * FROM unnest($1::text[], $2::bigint[]))";
        let rows: Vec<(
            MachineId,
            RackId,
            i32,
            Json<serde_json::Value>,
            ConfigVersion,
        )> = sqlx::query_as(query)
            .bind(rack_ids)
            .bind(slots)
            .fetch_all(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
        for (id, rack_id, slot_number, state, version) in rows {
            if !id.machine_type().is_host() {
                continue;
            }
            let machine_id = HostMachineId::try_from(id)
                .map_err(|e| DatabaseError::new(query, sqlx::Error::Decode(e.into())))?;
            let state = serde_json::from_value(state.0)
                .map_err(|e| DatabaseError::new(query, sqlx::Error::Decode(e.into())))?;
            insert(
                &mut inventory.compute,
                rack_id,
                slot_number,
                NvLinkDomainTray::Compute {
                    machine_id,
                    controller_state: Box::new(Versioned::new(state, version)),
                },
            )?;
        }
    }
    let (rack_ids, slots) = positions(members, RackCapabilityType::Switch);
    if !rack_ids.is_empty() {
        let query = "SELECT id, rack_id, slot_number, controller_state, controller_state_version
            FROM switches WHERE deleted IS NULL AND (rack_id, slot_number) IN
            (SELECT * FROM unnest($1::text[], $2::bigint[]))";
        let rows: Vec<(
            SwitchId,
            RackId,
            i32,
            Json<SwitchControllerState>,
            ConfigVersion,
        )> = sqlx::query_as(query)
            .bind(rack_ids)
            .bind(slots)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
        for (switch_id, rack_id, slot_number, state, version) in rows {
            insert(
                &mut inventory.switches,
                rack_id,
                slot_number,
                NvLinkDomainTray::Switch {
                    switch_id,
                    controller_state: Box::new(Versioned::new(state.0, version)),
                },
            )?;
        }
    }
    Ok(inventory)
}

/// Resolve every Domain member, using RackGroup only to map rack index to rack ID.
/// Device identity and state come from actual inventory, matched by slot_number (SlotID).
/// Missing trays remain None; invalid rack indexes and ambiguous inventory are errors.
/// Compute and switch inventory are each read in one batch in the caller's transaction.
pub async fn resolve_domain_members(
    txn: &mut PgConnection,
    domain_id: Uuid,
) -> DatabaseResult<Vec<ResolvedNvLinkDomainMember>> {
    let domain = super::find_by_ids(&mut *txn, &[domain_id])
        .await?
        .pop()
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "nvlink_domain",
            id: domain_id.to_string(),
        })?;
    let group = crate::expected_rack_group::find_by_rack_group_id(&mut *txn, &domain.rack_group_id)
        .await?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "expected_rack_group",
            id: domain.rack_group_id.to_string(),
        })?;
    let mut members = domain
        .members
        .into_iter()
        .map(|member| {
            let rack = usize::try_from(member.rack_index)
                .ok()
                .and_then(|index| group.racks.get(index))
                .ok_or_else(|| {
                    DatabaseError::FailedPrecondition(format!(
                        "domain {domain_id} rack index {} is outside rack group {}",
                        member.rack_index, group.rack_group_id
                    ))
                })?;
            if member.component_type == RackCapabilityType::PowerShelf {
                return Err(DatabaseError::InvalidArgument(
                    "domain trays must be Compute or Switch".into(),
                ));
            }
            Ok(ResolvedNvLinkDomainMember {
                member,
                rack_id: rack.rack_id.clone(),
                tray: None,
            })
        })
        .collect::<DatabaseResult<Vec<_>>>()?;
    let positions: Vec<_> = members
        .iter()
        .map(|member| TrayPosition {
            rack_id: member.rack_id.clone(),
            component_type: member.member.component_type.clone(),
            slot_index: member.member.slot_index,
        })
        .collect();
    let inventory = load_inventory(txn, &positions).await?;
    for (member, position) in members.iter_mut().zip(&positions) {
        member.tray = inventory.get(position);
    }
    Ok(members)
}

/// Find an actual tray by rack, component type, and slot_number (SlotID).
/// Returns None if undiscovered; rejects PowerShelf and multiple active matches.
pub async fn find_tray(
    txn: &mut PgConnection,
    rack_id: &RackId,
    component_type: RackCapabilityType,
    slot_index: u32,
) -> DatabaseResult<Option<NvLinkDomainTray>> {
    if component_type == RackCapabilityType::PowerShelf {
        return Err(DatabaseError::InvalidArgument(
            "domain trays must be Compute or Switch".into(),
        ));
    }
    let position = TrayPosition {
        rack_id: rack_id.clone(),
        slot_index,
        component_type,
    };
    let inventory = load_inventory(txn, std::slice::from_ref(&position)).await?;
    Ok(inventory.get(&position))
}

#[cfg(test)]
mod tests;
