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

use ::rpc::forge as rpc;

use super::args::{MaintenanceOptions, TerminateOptions};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

fn resolve_firmware_upgrade_source(
    args: &MaintenanceOptions,
) -> CarbideCliResult<(String, Option<String>)> {
    let explicit_firmware_upgrade = args.activities.as_ref().is_some_and(|activities| {
        activities
            .iter()
            .any(|activity| activity == "firmware-upgrade")
    });
    let explicit_nvos_update = args
        .activities
        .as_ref()
        .is_some_and(|activities| activities.iter().any(|activity| activity == "nvos-update"));
    let requires_firmware_object_json = explicit_firmware_upgrade || explicit_nvos_update;

    if args.firmware_version.is_some() && args.sot_json_file.is_some() {
        return Err(CarbideCliError::ChooseOneError(
            "--firmware-version",
            "--sot-json-file",
        ));
    }

    let firmware_version = if let Some(path) = args.sot_json_file.as_ref() {
        let config_json = std::fs::read_to_string(path)?;
        serde_json::from_str::<serde_json::Value>(&config_json)?;
        config_json
    } else {
        args.firmware_version.clone().unwrap_or_default()
    };

    let access_token = args.access_token.as_ref().and_then(|token| {
        if token.trim().is_empty() {
            None
        } else {
            Some(token.clone())
        }
    });

    if requires_firmware_object_json && firmware_version.trim().is_empty() {
        return Err(CarbideCliError::GenericError(
            "--activities firmware-upgrade/nvos-update requires SOT JSON from --sot-json-file or --firmware-version"
                .to_string(),
        ));
    }
    if !requires_firmware_object_json && args.sot_json_file.is_some() {
        return Err(CarbideCliError::GenericError(
            "--sot-json-file requires --activities firmware-upgrade or nvos-update".to_string(),
        ));
    }
    if !requires_firmware_object_json && args.firmware_version.is_some() {
        return Err(CarbideCliError::GenericError(
            "--firmware-version requires --activities firmware-upgrade or nvos-update".to_string(),
        ));
    }
    if !requires_firmware_object_json && args.access_token.is_some() {
        return Err(CarbideCliError::GenericError(
            "--access-token requires --activities firmware-upgrade or nvos-update".to_string(),
        ));
    }
    if requires_firmware_object_json && args.firmware_version.is_some() {
        serde_json::from_str::<serde_json::Value>(&firmware_version)?;
    }

    Ok((firmware_version, access_token))
}

pub(super) async fn on_demand_rack_maintenance(
    api_client: &ApiClient,
    args: MaintenanceOptions,
) -> CarbideCliResult<()> {
    use rpc::maintenance_activity_config::Activity as ProtoActivity;

    let (firmware_version, access_token) = resolve_firmware_upgrade_source(&args)?;
    let components = args.components.unwrap_or_default();
    let force_update = args.force_update;

    let activities: Vec<rpc::MaintenanceActivityConfig> = args
        .activities
        .unwrap_or_default()
        .iter()
        .map(|s| {
            let activity = match s.as_str() {
                "firmware-upgrade" => {
                    Ok(ProtoActivity::FirmwareUpgrade(rpc::FirmwareUpgradeActivity {
                        firmware_version: firmware_version.clone(),
                        components: components.clone(),
                        access_token: access_token.clone(),
                        force_update,
                    }))
                }
                "nvos-update" => Ok(ProtoActivity::NvosUpdate(rpc::NvosUpdateActivity {
                    config_json: firmware_version.clone(),
                    access_token: access_token.clone(),
                })),
                "configure-nmx-cluster" => Ok(ProtoActivity::ConfigureNmxCluster(
                    rpc::ConfigureNmxClusterActivity {},
                )),
                "power-sequence" => Ok(ProtoActivity::PowerSequence(
                    rpc::PowerSequenceActivity {},
                )),
                other => Err(eyre::eyre!(
                    "unknown activity '{}'. valid values: firmware-upgrade, nvos-update, configure-nmx-cluster, power-sequence",
                    other
                )),
            }?;
            Ok::<_, eyre::Report>(rpc::MaintenanceActivityConfig {
                activity: Some(activity),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    api_client
        .on_demand_rack_maintenance(
            args.rack,
            args.machine_ids.unwrap_or_default(),
            args.switch_ids.unwrap_or_default(),
            args.power_shelf_ids.unwrap_or_default(),
            activities,
        )
        .await?;
    println!("On-demand rack maintenance scheduled successfully.");
    Ok(())
}

pub(super) async fn terminate_rack_maintenance(
    api_client: &ApiClient,
    args: TerminateOptions,
) -> CarbideCliResult<()> {
    api_client.terminate_rack_maintenance(args.rack).await?;
    println!(
        "Rack maintenance termination requested. The rack will transition to Error; externally submitted work is not guaranteed to stop."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use carbide_uuid::rack::RackId;

    use super::*;

    fn options() -> MaintenanceOptions {
        MaintenanceOptions {
            rack: RackId::new("rack-test"),
            machine_ids: None,
            switch_ids: None,
            power_shelf_ids: None,
            activities: None,
            firmware_version: None,
            sot_json_file: None,
            access_token: None,
            force_update: false,
            components: None,
        }
    }

    // resolve_firmware_upgrade_source decides the SOT firmware source and access
    // token for a set of MaintenanceOptions: a firmware-upgrade activity demands a
    // SOT JSON, inline JSON must actually parse, an empty access token reads as
    // absent, and --firmware-version/--access-token are rejected without an
    // activity that uses them. The error type is not PartialEq, so failures use
    // Fails (+ map_err(drop)); successful rows yield the (firmware_version,
    // access_token) tuple the originals asserted on.
    #[test]
    fn resolve_firmware_upgrade_source_cases() {
        scenarios!(
            run = |args| resolve_firmware_upgrade_source(&args).map_err(drop);
            "firmware-upgrade without a SOT JSON source is rejected" {
                MaintenanceOptions {
                    activities: Some(vec!["firmware-upgrade".to_string()]),
                    access_token: Some("token".to_string()),
                    ..options()
                } => Fails,
            }

            "firmware-upgrade with inline JSON allows a missing access token" {
                MaintenanceOptions {
                    activities: Some(vec!["firmware-upgrade".to_string()]),
                    firmware_version: Some(r#"{"Id":"fw"}"#.to_string()),
                    ..options()
                } => Yields((r#"{"Id":"fw"}"#.to_string(), None)),
            }

            "firmware-upgrade treats an empty access token as missing" {
                MaintenanceOptions {
                    activities: Some(vec!["firmware-upgrade".to_string()]),
                    firmware_version: Some(r#"{"Id":"fw"}"#.to_string()),
                    access_token: Some(String::new()),
                    ..options()
                } => Yields((r#"{"Id":"fw"}"#.to_string(), None)),
            }

            "firmware-upgrade rejects invalid inline JSON" {
                MaintenanceOptions {
                    activities: Some(vec!["firmware-upgrade".to_string()]),
                    firmware_version: Some("not-json".to_string()),
                    access_token: Some("token".to_string()),
                    ..options()
                } => Fails,
            }

            "--firmware-version without a firmware-upgrade activity is rejected" {
                MaintenanceOptions {
                    firmware_version: Some(r#"{"Id":"fw"}"#.to_string()),
                    ..options()
                } => Fails,
            }

            "--access-token without a firmware-upgrade activity is rejected" {
                MaintenanceOptions {
                    access_token: Some("token".to_string()),
                    ..options()
                } => Fails,
            }
        );
    }
}
