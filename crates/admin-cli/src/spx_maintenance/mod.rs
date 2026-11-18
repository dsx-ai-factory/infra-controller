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

//! Requested maintenance state for Spectrum-X planes.

mod args;

pub(crate) use args::Args;
use args::{Action, MaintenanceMode, PlaneSelector, RailSelector};
use config_version::ConfigVersion;
use model::machine::astra::ASTRA_NIC_MAPPINGS;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge;

use crate::cfg::dispatch::dispatch_via_run;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::{async_write, async_writeln};

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let configuration = match self.action {
            Action::Show => {
                tracing::info!(machine_id = %self.machine_id, "spx-maintenance: [cli] CLI requesting SPX maintenance state from Core");
                ctx.api_client
                    .0
                    .get_machine_spx_multiplane(forge::MachineSpxMultiplaneRequest {
                        machine_id: Some(self.machine_id),
                    })
                    .await?
            }
            Action::Multiplane {
                rail,
                plane,
                mode,
                reference,
                ..
            } => {
                tracing::info!(machine_id = %self.machine_id, ?rail, ?plane, maintenance_mode = mode == MaintenanceMode::On, reference_supplied = reference.is_some(), "spx-maintenance: [cli] CLI sending SPX maintenance update to Core");
                let rail_selector = match rail {
                    RailSelector::One(rail) => forge::machine_spx_multiplane_maintenance_update_request::RailSelector::Rail(rail.into()),
                    RailSelector::Range { start, end } => forge::machine_spx_multiplane_maintenance_update_request::RailSelector::RailRange(forge::SpxMultiplaneRange { start: Some(start.into()), end: Some(end.into()) }),
                    RailSelector::All => forge::machine_spx_multiplane_maintenance_update_request::RailSelector::AllRails(()),
                };
                let plane_selector = match plane {
                    PlaneSelector::One(plane) => forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::Plane(plane.into()),
                    PlaneSelector::Range { start, end } => forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::PlaneRange(forge::SpxMultiplaneRange { start: Some(start.into()), end: Some(end.into()) }),
                    PlaneSelector::All => forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::AllPlanes(()),
                };
                ctx.api_client
                    .0
                    .update_machine_spx_multiplane_maintenance(
                        forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
                            machine_id: Some(self.machine_id),
                            rail_selector: Some(rail_selector),
                            plane_selector: Some(plane_selector),
                            maintenance_mode: Some(mode == MaintenanceMode::On),
                            reference,
                        },
                    )
                    .await?
            }
        };
        tracing::info!(machine_id = %self.machine_id, interface_count = configuration.interfaces.len(), dpu_count = configuration.dpu_statuses.len(), "spx-maintenance: [cli] CLI received SPX maintenance state from Core");
        if ctx.config.format == OutputFormat::Json {
            async_writeln!(
                ctx.output_file,
                "{}",
                serde_json::to_string_pretty(&configuration)?
            )?;
        } else {
            async_writeln!(
                ctx.output_file,
                "Requested maintenance configuration for host {}",
                self.machine_id
            )?;
            let mut table = Table::new();
            table.set_titles(row![
                "Logical interface",
                "Rail",
                "Plane",
                "Requested maintenance mode",
                "Reference",
                "Revision",
                "Config updated at (UTC)"
            ]);
            for state in &configuration.interfaces {
                // The desired revision already carries its last-change timestamp. Unset modes
                // have only an initialization revision, so do not present that as an operator change.
                let updated_at = state
                    .cfg_maintenance_mode
                    .and_then(|_| state.revision.parse::<ConfigVersion>().ok())
                    .map(|revision| revision.timestamp().format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| "-".to_string());
                table.add_row(row![
                    state.logical_ifname,
                    state.rail,
                    state.plane,
                    match state.cfg_maintenance_mode {
                        Some(true) => "on",
                        Some(false) => "off",
                        None => "unset",
                    },
                    state.reference.as_deref().unwrap_or_default(),
                    state.revision,
                    updated_at,
                ]);
            }
            async_write!(ctx.output_file, "{}", table)?;
            if configuration.dpu_statuses.is_empty() {
                async_writeln!(ctx.output_file, "No BF4 Astra DPUs present.")?;
            }
            for dpu in &configuration.dpu_statuses {
                async_writeln!(
                    ctx.output_file,
                    "DPU {}",
                    dpu.dpu_machine_id
                        .map(|id| id.to_string())
                        .unwrap_or_default()
                )?;
                let mut table = Table::new();
                table.set_titles(row![
                    "Logical interface",
                    "Physical interface",
                    "Applied maintenance mode",
                    "Observed revision",
                    "Status",
                    "Netdev",
                    "Observed at",
                    "Error"
                ]);
                for observed in &dpu.interfaces {
                    let desired = configuration
                        .interfaces
                        .iter()
                        .find(|state| state.logical_ifname == observed.logical_ifname);
                    let status = match desired {
                        Some(desired)
                            if observed.observed_at.is_some()
                                && observed.revision == desired.revision =>
                        {
                            if observed.error.is_some() {
                                "Error"
                            } else if desired.cfg_maintenance_mode.is_none()
                                && observed.maintenance_mode.is_some()
                            {
                                "Observed"
                            } else if observed.maintenance_mode.is_some()
                                && observed.maintenance_mode == desired.cfg_maintenance_mode
                            {
                                "Applied"
                            } else {
                                "Pending"
                            }
                        }
                        _ => "Pending",
                    };
                    table.add_row(row![
                        observed.logical_ifname,
                        observed
                            .netdev
                            .as_deref()
                            .and_then(|netdev| ASTRA_NIC_MAPPINGS
                                .iter()
                                .find(|mapping| mapping.dpu_original_ifname == netdev))
                            .map(|mapping| mapping.physical_ifname)
                            .unwrap_or_default(),
                        match observed.maintenance_mode {
                            Some(true) => "on",
                            Some(false) => "off",
                            None => "unknown",
                        },
                        observed.revision,
                        status,
                        observed.netdev.as_deref().unwrap_or_default(),
                        observed
                            .observed_at
                            .and_then(|timestamp| chrono::DateTime::<chrono::Utc>::try_from(
                                timestamp
                            )
                            .ok())
                            .map(|timestamp| timestamp.to_rfc3339())
                            .unwrap_or_default(),
                        observed.error.as_deref().unwrap_or_default(),
                    ]);
                }
                async_write!(ctx.output_file, "{}", table)?;
            }
        }
        Ok(())
    }
}

dispatch_via_run!(Args);
