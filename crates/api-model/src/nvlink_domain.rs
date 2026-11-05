/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Domain configuration, membership, and durable fabric-controller progress.

use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::{RackGroupId, RackId};
use carbide_uuid::switch::SwitchId;
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};
use uuid::Uuid;

use crate::controller_outcome::PersistentStateHandlerOutcome;
use crate::metadata::Metadata;

/// Component roles that participate in an NVLink fabric; power shelves are Rack resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text")]
pub enum NvLinkDomainComponentType {
    /// Compute tray.
    Compute,
    /// NVSwitch tray.
    Switch,
}

/// Concrete inventory identity resolved from a Domain member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NvLinkDomainTray {
    /// A discovered compute tray.
    Compute(MachineId),
    /// A discovered NVSwitch tray.
    Switch(SwitchId),
}

/// Member resolution preserves undiscovered members rather than dropping them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvLinkDomainMemberResolution {
    /// Original declaration to correlate with readiness and operator output.
    pub member: NvLinkDomainMember,
    /// Unique discovered tray, or absence while inventory is incomplete.
    pub tray: Option<NvLinkDomainTray>,
}

/// Physical tray position selected by a Domain declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, FromRow)]
pub struct NvLinkDomainMember {
    /// Role used to select the compute or switch inventory.
    pub component_type: NvLinkDomainComponentType,
    /// Physical Rack containing the tray position.
    pub rack_id: RackId,
    /// Non-negative tray index using the RMS/NMX-C numbering space for this role.
    pub tray_index: i32,
}

/// Caller-owned Domain configuration, distinct from observed fabric identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvLinkDomainConfig {
    /// Physical RackGroup containing all selected trays.
    pub rack_group_id: RackGroupId,
    /// Site-configured fabric profile, independent of the physical Rack profile.
    pub domain_profile_id: String,
    /// Explicit compute and switch membership, independent of discovery order.
    pub members: Vec<NvLinkDomainMember>,
}

/// Resumable fabric lifecycle persisted between controller iterations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NvLinkDomainState {
    /// Await resolution and readiness of every declared member.
    Discovering,
    /// Fabric configuration has completed.
    Ready,
    /// Tear down fabric state before releasing member claims.
    Deleting,
}

/// A single managed Domain resource containing intent and observed lifecycle state.
#[derive(Clone, Debug)]
pub struct NvLinkDomain {
    /// Stable Core resource identity; not the observed NMX-C cluster UUID.
    pub id: Uuid,
    /// Operator-supplied configuration.
    pub config: NvLinkDomainConfig,
    /// Operator-visible name, description, and labels.
    pub metadata: Metadata,
    /// Optimistic concurrency token for configuration changes.
    pub version: ConfigVersion,
    /// Independently versioned controller progress.
    pub controller_state: Versioned<NvLinkDomainState>,
    /// Last controller iteration outcome, absent before the first iteration.
    pub controller_state_outcome: Option<PersistentStateHandlerOutcome>,
    /// Observed fabric UUID, absent until NMX-C reports it.
    pub nmxc_domain_id: Option<Uuid>,
    /// Whether RackGroup automation owns the declaration.
    pub managed: bool,
    /// Creation time.
    pub created: DateTime<Utc>,
    /// Last persisted change time.
    pub updated: DateTime<Utc>,
    /// Deletion request time; members remain claimed until teardown completes.
    pub deleted: Option<DateTime<Utc>>,
}

impl<'r> FromRow<'r, PgRow> for NvLinkDomain {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let members: sqlx::types::Json<Vec<NvLinkDomainMember>> = row.try_get("members")?;
        let labels: sqlx::types::Json<_> = row.try_get("metadata_labels")?;
        let state: sqlx::types::Json<NvLinkDomainState> = row.try_get("controller_state")?;
        let outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
            row.try_get("controller_state_outcome")?;
        Ok(Self {
            id: row.try_get("id")?,
            config: NvLinkDomainConfig {
                rack_group_id: row.try_get("rack_group_id")?,
                domain_profile_id: row.try_get("domain_profile_id")?,
                members: members.0,
            },
            metadata: Metadata {
                name: row.try_get("metadata_name")?,
                description: row.try_get("metadata_description")?,
                labels: labels.0,
            },
            version: row.try_get("version")?,
            controller_state: Versioned::new(state.0, row.try_get("controller_state_version")?),
            controller_state_outcome: outcome.map(|value| value.0),
            nmxc_domain_id: row.try_get("nmxc_domain_id")?,
            managed: row.try_get("managed")?,
            created: row.try_get("created")?,
            updated: row.try_get("updated")?,
            deleted: row.try_get("deleted")?,
        })
    }
}
