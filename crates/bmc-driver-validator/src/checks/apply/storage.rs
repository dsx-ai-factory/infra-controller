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

//! The boot controller cleanup NICo runs: erasing it, then creating the
//! boot volume.

use bmc_platform::{OpCx, PlatformError};

use crate::checks::context::CheckCx;
use crate::checks::errors::{describe, failed};
use crate::checks::model::{Outcome, Step};
use crate::checks::ops::{Attempt, Effect, how, until_answered};

/// The volume NICo's cleanup creates on the boot controller.
const BOOT_VOLUME: &str = "VD_0";

/// Erases the boot controller and waits until it lists no volume.
pub(super) async fn erase_boot_controller(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let storage = bmc.storage()?;
    let controller = boot_controller(cx).await?;
    let outcome = until_answered(
        |step| cx.trace(step),
        async || {
            storage
                .decommission_controller(&controller)
                .await
                .map_err(failed("decommission_controller"))
        },
    )
    .await?;
    cx.trace(format!("decommission_controller: {}", how(&outcome)));
    cx.read_back(&outcome, Effect::Immediate, async || {
        match volume_names(cx, &controller).await {
            Ok(names) if names.is_empty() => Attempt::Done(()),
            Ok(names) => Attempt::NotYet(format!("{controller} lists {}", names.join(", "))),
            Err(error) => Attempt::NotYet(describe("reading volumes", &error)),
        }
    })
    .await?;
    Ok(format!(
        "{controller} erased ({}); it lists no volume",
        how(&outcome)
    ))
}

/// Creates [`BOOT_VOLUME`] on the boot controller, restarts the host so the
/// job runs, and waits until the controller lists it.
pub(super) async fn create_boot_volume(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let storage = bmc.storage()?;
    let controller = boot_controller(cx).await?;
    let outcome = until_answered(
        |step| cx.trace(step),
        async || {
            storage
                .create_volume(&controller, BOOT_VOLUME)
                .await
                .map_err(failed("create_volume"))
        },
    )
    .await?;
    cx.trace(format!("create_volume: {}", how(&outcome)));
    cx.read_back(&outcome, Effect::OnRestart, async || {
        match volume_names(cx, &controller).await {
            Ok(names) if names.iter().any(|name| name == BOOT_VOLUME) => Attempt::Done(()),
            Ok(names) if names.is_empty() => {
                Attempt::NotYet(format!("{controller} lists no volume"))
            }
            Ok(names) => Attempt::NotYet(format!("{controller} lists {}", names.join(", "))),
            Err(error) => Attempt::NotYet(describe("reading volumes", &error)),
        }
    })
    .await?;
    Ok(format!(
        "{BOOT_VOLUME} created on {controller} ({}) and listed after a host restart",
        how(&outcome)
    ))
}

/// The boot controller the driver names; a check is skipped without one.
async fn boot_controller(cx: &CheckCx<'_>) -> Result<String, Outcome> {
    cx.bmc()
        .storage()?
        .boot_controller()
        .await
        .map_err(failed("boot_controller"))?
        .ok_or_else(|| Outcome::Skipped("the driver names no boot controller".to_string()))
}

/// The names of the volumes on storage controller `controller`, read over a
/// fresh connection rather than through the driver under test.
async fn volume_names(cx: &CheckCx<'_>, controller: &str) -> Result<Vec<String>, PlatformError> {
    let connection = cx.target().fresh_connection().await?;
    let bmc = cx.bmc();
    let op = OpCx::new(
        connection.bmc.as_ref(),
        connection.service_root.as_ref(),
        bmc.endpoint().identity(),
    );
    let storage = op
        .system()
        .await?
        .storage_controllers()
        .await
        .map_err(PlatformError::from_redfish)?
        .unwrap_or_default()
        .into_iter()
        .find(|storage| storage.raw().odata_id.last_segment() == Some(controller))
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: format!("the system lists no storage controller {controller}"),
        })?;
    let Some(volumes) = storage.volumes() else {
        return Ok(Vec::new());
    };
    Ok(volumes
        .members()
        .await
        .map_err(PlatformError::from_redfish)?
        .iter()
        .map(|volume| volume.raw().name.clone())
        .collect())
}
