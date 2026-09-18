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

//! State Controller implementation for Racks.

use carbide_uuid::rack::{RackId, RackProfileId};
use db::machine;
use model::machine::StableHostMachine;
use model::machine::machine_search_config::MachineSearchConfig;
use model::rack::Rack;
use model::rack_type::{RackCapabilitiesSet, RackProfile};
use sqlx::PgConnection;
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use crate::context::RackStateHandlerContextObjects;

pub mod config;
pub mod context;
pub mod created;
pub mod deleting;
pub mod discovering;
pub mod error_state;
pub mod fabric_manager;
pub mod firmware_object;
pub mod handler;
pub mod io;
pub mod maintenance;
pub mod metrics;
pub mod ready;
mod validating;
pub mod write_ops;

/// Loads all machines associated with the given rack via their `rack_id` FK.
pub(crate) async fn get_machines_from_rack(
    rack: &Rack,
    txn: &mut PgConnection,
) -> Result<Vec<StableHostMachine>, StateHandlerError> {
    let search_cfg = MachineSearchConfig {
        rack_id: Some(rack.id.clone()),
        ..Default::default()
    };

    let machine_ids = machine::find_machine_ids(&mut *txn, search_cfg).await?;
    let machines = machine::find(
        txn,
        db::ObjectFilter::List(&machine_ids),
        MachineSearchConfig::default(),
    )
    .await?;

    Ok(machines)
}

/// Resolves the `RackProfile` for a rack by looking up its `rack_profile_id`
/// from the runtime config. Returns `None` with a log message if the rack has
/// no `rack_profile_id` or the profile is unknown.
pub(crate) fn resolve_profile<'a>(
    id: &RackId,
    rack_profile_id: Option<&RackProfileId>,
    ctx: &'a StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Option<&'a RackProfile> {
    let rack_profile_id = match rack_profile_id {
        Some(rc) => rc,
        None => {
            tracing::info!(
                rack_id = %id,
                "Rack has no rack_profile_id configured",
            );
            return None;
        }
    };

    match ctx
        .services
        .site_config
        .rack_profiles
        .get(rack_profile_id.as_str())
    {
        Some(profile) => Some(profile),
        None => {
            tracing::warn!(
                rack_id = %id,
                rack_profile_id = %rack_profile_id,
                "Rack has unknown rack_profile_id",
            );
            None
        }
    }
}

/// Resolves the `RackCapabilitiesSet` for a rack. Convenience wrapper around
/// `resolve_profile` for callers that only need device counts.
pub(crate) fn resolve_capabilities<'a>(
    id: &RackId,
    rack_profile_id: Option<&RackProfileId>,
    ctx: &'a StateHandlerContext<'_, RackStateHandlerContextObjects>,
) -> Option<&'a RackCapabilitiesSet> {
    resolve_profile(id, rack_profile_id, ctx).map(|p| &p.rack_capabilities)
}
